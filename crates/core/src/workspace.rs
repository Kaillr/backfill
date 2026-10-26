//! Everything the calendar shows, and the edits it can make.

use std::collections::{HashMap, HashSet};

use jiff::{SignedDuration, civil::Date, tz::TimeZone};
use serde::{Deserialize, Serialize};

use crate::{
    Activity, ActivityId, Issue, IssueKey, JiraHistory, RemoteWorklog, RemoteWorklogId, Source, TimeRange,
    Worklog, WorklogId, fill,
    issue::find_issue_keys,
    suggest::{self, SuggestContext, Suggestion},
    sync::{self, SyncOp},
    time,
};

#[derive(Clone, Debug)]
pub struct Settings {
    /// Usual start of the workday, local time.
    pub day_start: (i8, i8),
    /// Hours a full day adds up to.
    pub target: SignedDuration,
    /// The smallest block worth logging on its own.
    pub min_block: SignedDuration,
    /// Drags and fills land on this grid.
    pub snap_minutes: i64,
    /// The Jira projects time is logged on, such as `APP`. Empty allows any project.
    pub projects: Vec<String>,
}

impl Settings {
    /// Whether time may be logged on `key`.
    pub fn allows(&self, key: &IssueKey) -> bool {
        self.projects.is_empty() || self.projects.iter().any(|p| p.eq_ignore_ascii_case(key.project()))
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            day_start: (8, 0),
            target: SignedDuration::from_mins(450),
            min_block: SignedDuration::from_mins(15),
            snap_minutes: 15,
            projects: Vec::new(),
        }
    }
}

/// What "fill day" would log.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FillPlan {
    /// Meetings that name an issue, each logged on that issue.
    pub meetings: Vec<(ActivityId, IssueKey, TimeRange)>,
    /// The rest, logged on the primary task.
    pub gaps: Vec<TimeRange>,
}

impl FillPlan {
    pub fn is_empty(&self) -> bool {
        self.meetings.is_empty() && self.gaps.is_empty()
    }

    pub fn total(&self) -> SignedDuration {
        let meetings = self.meetings.iter().map(|(_, _, r)| r.duration());
        meetings.chain(self.gaps.iter().map(|r| r.duration())).sum()
    }
}

/// How a worklog compares with what Jira has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    Synced,
    New,
    Changed,
}

pub struct Workspace {
    pub tz: TimeZone,
    pub settings: Settings,
    pub issues: Vec<Issue>,
    pub activities: Vec<Activity>,
    /// Meetings that aren't shown because they were cancelled or declined. They're kept
    /// to recognise Toggl entries made from them, whatever language a cancellation is in.
    pub skipped: Vec<Activity>,
    /// Local, desired state.
    pub worklogs: Vec<Worklog>,
    /// What Jira had at the last pull.
    pub remote: Vec<RemoteWorklog>,
    /// Issues recently opened in Jira, most recent first.
    pub viewed: Vec<IssueKey>,
    /// Absorb choices made for recurring series, so occurrences loaded later follow them.
    series_absorbed: HashMap<String, bool>,
    /// Absorb choices made for one-off meetings, so they survive a reload.
    activity_absorbed: HashMap<ActivityId, bool>,
    /// Worklogs edited here that Jira also changed since the last pull. Syncing would
    /// overwrite Jira's change, so the review says so.
    pub conflicts: HashSet<WorklogId>,
    /// What Jira says you were assigned and moved, week by week.
    pub history: JiraHistory,
    next_id: u64,
}

/// The part of a workspace kept between runs. Activities aren't in it: they are read
/// again from Outlook, Toggl and git.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Saved {
    pub worklogs: Vec<Worklog>,
    pub remote: Vec<RemoteWorklog>,
    pub issues: Vec<Issue>,
    pub viewed: Vec<IssueKey>,
    pub series_absorbed: HashMap<String, bool>,
    pub activity_absorbed: HashMap<ActivityId, bool>,
    pub conflicts: HashSet<WorklogId>,
    #[serde(default)]
    pub history: JiraHistory,
    pub next_id: u64,
}

impl Workspace {
    pub fn new(tz: TimeZone, settings: Settings) -> Self {
        Self {
            tz,
            settings,
            issues: Vec::new(),
            activities: Vec::new(),
            skipped: Vec::new(),
            worklogs: Vec::new(),
            remote: Vec::new(),
            viewed: Vec::new(),
            series_absorbed: HashMap::new(),
            activity_absorbed: HashMap::new(),
            conflicts: HashSet::new(),
            history: JiraHistory::default(),
            next_id: 1,
        }
    }

    pub fn save(&self) -> Saved {
        Saved {
            worklogs: self.worklogs.clone(),
            remote: self.remote.clone(),
            issues: self.issues.clone(),
            viewed: self.viewed.clone(),
            series_absorbed: self.series_absorbed.clone(),
            activity_absorbed: self.activity_absorbed.clone(),
            conflicts: self.conflicts.clone(),
            history: self.history.clone(),
            next_id: self.next_id,
        }
    }

    pub fn restore(&mut self, saved: Saved) {
        self.worklogs = saved.worklogs;
        self.remote = saved.remote;
        self.issues = saved.issues;
        self.viewed = saved.viewed;
        self.series_absorbed = saved.series_absorbed;
        self.activity_absorbed = saved.activity_absorbed;
        self.conflicts = saved.conflicts;
        self.history = saved.history;
        self.next_id = saved.next_id.max(1);
    }

    /// Adds or refreshes issues, keeping any already known that aren't in `issues`.
    pub fn upsert_issues(&mut self, issues: impl IntoIterator<Item = Issue>) {
        for issue in issues {
            match self.issues.iter_mut().find(|i| i.key == issue.key) {
                Some(known) => *known = issue,
                None => self.issues.push(issue),
            }
        }
    }

    /// Takes in what Jira has now for worklogs starting inside `window`, and reconciles
    /// it with what's here.
    ///
    /// - Unchanged here: Jira's version wins, including deletions made in Jira.
    /// - Edited here: the edit is kept and will go to Jira on the next sync. If Jira
    ///   changed it too, it's recorded in [`Workspace::conflicts`]. If Jira deleted it, the
    ///   edit will be created again.
    /// - Deleted here: stays deleted, and the next sync deletes it in Jira.
    /// - New in Jira: added here.
    pub fn merge_pull(&mut self, window: TimeRange, fresh: Vec<RemoteWorklog>) {
        let in_window = |r: &RemoteWorklog| window.contains(r.range.start);
        let old: HashMap<RemoteWorklogId, RemoteWorklog> =
            self.remote.iter().filter(|r| in_window(r)).map(|r| (r.id.clone(), r.clone())).collect();
        let fresh: Vec<RemoteWorklog> = fresh.into_iter().filter(|r| in_window(r)).collect();
        let fresh_by_id: HashMap<RemoteWorklogId, RemoteWorklog> =
            fresh.iter().map(|r| (r.id.clone(), r.clone())).collect();

        let mut removed = Vec::new();
        for w in &mut self.worklogs {
            let Some(id) = w.remote_id.clone() else { continue };
            let (before, now) = (old.get(&id), fresh_by_id.get(&id));
            if before.is_none() && now.is_none() {
                continue;
            }
            let edited_here = before.is_some_and(|b| w.differs_from(b));
            match (now, edited_here) {
                (Some(now), false) => {
                    w.issue = now.issue.clone();
                    w.range = now.range;
                    w.comment = now.comment.clone();
                }
                (Some(now), true) => {
                    if before.is_some_and(|b| b != now) {
                        self.conflicts.insert(w.id);
                    }
                }
                (None, false) => removed.push(w.id),
                (None, true) => w.remote_id = None,
            }
        }
        self.worklogs.retain(|w| !removed.contains(&w.id));

        let known: HashSet<RemoteWorklogId> = self.worklogs.iter().filter_map(|w| w.remote_id.clone()).collect();
        for r in &fresh {
            // In Jira but not here: new in Jira, unless it was deleted here since the last pull.
            if !known.contains(&r.id) && !old.contains_key(&r.id) {
                let id = WorklogId(self.next_id());
                self.worklogs.push(Worklog {
                    id,
                    remote_id: Some(r.id.clone()),
                    issue: r.issue.clone(),
                    range: r.range,
                    comment: r.comment.clone(),
                    origin: None,
                });
            }
        }

        self.remote.retain(|r| !in_window(r));
        self.remote.extend(fresh);
        let remote_ids: HashSet<RemoteWorklogId> = self.remote.iter().map(|r| r.id.clone()).collect();
        let worklogs = &self.worklogs;
        self.conflicts.retain(|id| worklogs.iter().any(|w| w.id == *id && w.remote_id.as_ref().is_some_and(|r| remote_ids.contains(r))));
    }

    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn issue(&self, key: &IssueKey) -> Option<&Issue> {
        self.issues.iter().find(|i| &i.key == key)
    }

    pub fn activity(&self, id: ActivityId) -> Option<&Activity> {
        self.activities.iter().find(|a| a.id == id)
    }

    /// The time `activity` takes on each day it touches. See [`Activity::day_parts`].
    pub fn day_parts(&self, activity: &Activity) -> Vec<TimeRange> {
        let (h, m) = self.settings.day_start;
        let open = jiff::civil::time(h, m, 0, 0);
        let close = open.saturating_add(self.settings.target);
        activity.day_parts(&self.tz, (open, close))
    }

    /// The issue an activity's title names, among the projects time may be logged on.
    /// "SUP-55 support" names nothing when only APP is allowed.
    pub fn issue_hint(&self, activity: &Activity) -> Option<IssueKey> {
        find_issue_keys(&activity.title).into_iter().find(|key| self.settings.allows(key))
    }

    pub fn worklog(&self, id: WorklogId) -> Option<&Worklog> {
        self.worklogs.iter().find(|w| w.id == id)
    }

    pub fn day(&self, date: Date) -> TimeRange {
        time::day_range(date, &self.tz)
    }

    /// What happened on `date`. A Toggl entry copied from a meeting is left out: the
    /// meeting shows instead, or nothing does when the meeting was cancelled or declined.
    pub fn activities_on(&self, date: Date) -> impl Iterator<Item = &Activity> {
        let day = self.day(date);
        let on_day: Vec<&Activity> = self.activities.iter().filter(|a| a.range.overlaps(&day)).collect();
        let meetings: Vec<&Activity> = on_day
            .iter()
            .copied()
            .filter(|a| a.source == Source::Outlook)
            .chain(self.skipped.iter().filter(|a| a.range.overlaps(&day)))
            .collect();
        on_day.into_iter().filter(move |a| !meetings.iter().any(|m| a.copies(m)))
    }

    /// Replaces the skipped meetings that start inside `window`.
    pub fn replace_skipped(&mut self, window: TimeRange, incoming: Vec<Activity>) {
        self.skipped.retain(|a| !window.contains(a.range.start));
        self.skipped.extend(incoming);
    }

    pub fn worklogs_on(&self, date: Date) -> impl Iterator<Item = &Worklog> {
        let day = self.day(date);
        self.worklogs.iter().filter(move |w| w.range.overlaps(&day))
    }

    /// Worklogs Jira still has but that were deleted here, so the calendar can show what a
    /// sync would remove.
    pub fn deleted_on(&self, date: Date) -> Vec<&RemoteWorklog> {
        let day = self.day(date);
        self.remote
            .iter()
            .filter(|r| r.range.overlaps(&day))
            .filter(|r| !self.worklogs.iter().any(|w| w.remote_id.as_ref() == Some(&r.id)))
            .collect()
    }

    pub fn logged_on(&self, date: Date) -> SignedDuration {
        let day = self.day(date);
        self.worklogs
            .iter()
            .filter_map(|w| w.range.clip(&day))
            .map(|r| r.duration())
            .sum()
    }

    pub fn sync_state(&self, worklog: &Worklog) -> SyncState {
        let remote = worklog
            .remote_id
            .as_ref()
            .and_then(|id| self.remote.iter().find(|r| &r.id == id));
        match remote {
            None => SyncState::New,
            Some(r) if worklog.differs_from(r) => SyncState::Changed,
            Some(_) => SyncState::Synced,
        }
    }

    /// Jira's copy of a worklog that was changed here, as it was at the last pull.
    pub fn jira_copy(&self, worklog: &Worklog) -> Option<&RemoteWorklog> {
        let remote = self.remote.iter().find(|r| worklog.remote_id.as_ref() == Some(&r.id))?;
        worklog.differs_from(remote).then_some(remote)
    }

    /// Undoes one pending change, so Jira and here agree on it again: a new worklog goes,
    /// a changed one goes back to Jira's version, and a deleted one comes back.
    pub fn revert(&mut self, op: &SyncOp) {
        match op {
            SyncOp::Create(worklog) => self.worklogs.retain(|w| w.id != worklog.id),
            SyncOp::Update { before, after } | SyncOp::Move { before, after } => {
                if let Some(worklog) = self.worklogs.iter_mut().find(|w| w.id == after.id) {
                    worklog.issue = before.issue.clone();
                    worklog.range = before.range;
                    worklog.comment = before.comment.clone();
                }
                self.conflicts.remove(&after.id);
            }
            SyncOp::Delete(before) => {
                let id = WorklogId(self.next_id());
                self.worklogs.push(Worklog {
                    id,
                    remote_id: Some(before.id.clone()),
                    issue: before.issue.clone(),
                    range: before.range,
                    comment: before.comment.clone(),
                    origin: None,
                });
            }
        }
    }

    /// The worklog an activity was linked to, if it has been.
    pub fn worklog_for(&self, activity: ActivityId) -> Option<&Worklog> {
        self.worklogs.iter().find(|w| w.origin == Some(activity))
    }

    pub fn add_worklog(
        &mut self,
        issue: IssueKey,
        range: TimeRange,
        origin: Option<ActivityId>,
    ) -> WorklogId {
        let id = WorklogId(self.next_id());
        self.worklogs.push(Worklog {
            id,
            remote_id: None,
            issue,
            range,
            comment: String::new(),
            origin,
        });
        id
    }

    /// Logs an activity's time on `issue`. Linking it again moves the existing worklog to
    /// the new issue instead of logging the time twice.
    ///
    /// The time comes out of any plain worklog it overlaps, such as a filled-in main task,
    /// so logging a meeting separately doesn't push the day past its hours.
    pub fn link(&mut self, activity: ActivityId, issue: IssueKey) -> Option<WorklogId> {
        let parts = self.day_parts(self.activity(activity)?);
        if let Some(a) = self.activities.iter_mut().find(|a| a.id == activity) {
            a.absorbed = false;
        }
        let mut linked = None;
        for existing in self.worklogs.iter_mut().filter(|w| w.origin == Some(activity)) {
            existing.issue = issue.clone();
            linked.get_or_insert(existing.id);
        }
        if linked.is_some() {
            return linked;
        }
        // One worklog per day, so a meeting over several days logs each day's part.
        for part in parts {
            self.carve(part);
            linked.get_or_insert(self.add_worklog(issue.clone(), part, Some(activity)));
        }
        linked
    }

    /// Cuts `range` out of plain worklogs (those not tied to an activity), splitting one in
    /// two when the range falls in its middle.
    fn carve(&mut self, range: TimeRange) {
        let overlapping: Vec<Worklog> = self
            .worklogs
            .iter()
            .filter(|w| w.origin.is_none() && w.range.overlaps(&range))
            .cloned()
            .collect();
        for w in overlapping {
            let before = (w.range.start < range.start).then(|| TimeRange::new(w.range.start, range.start));
            let after = (w.range.end > range.end).then(|| TimeRange::new(range.end, w.range.end));
            match (before, after) {
                (None, None) => {
                    self.delete_worklog(w.id);
                }
                (Some(part), None) | (None, Some(part)) => self.set_range(w.id, part),
                (Some(first), Some(second)) => {
                    self.set_range(w.id, first);
                    self.add_worklog(w.issue.clone(), second, None);
                }
            }
        }
    }

    pub fn set_issue(&mut self, worklog: WorklogId, issue: IssueKey) {
        if let Some(w) = self.worklogs.iter_mut().find(|w| w.id == worklog) {
            w.issue = issue;
        }
    }

    pub fn set_range(&mut self, worklog: WorklogId, range: TimeRange) {
        if let Some(w) = self.worklogs.iter_mut().find(|w| w.id == worklog) {
            w.range = range;
        }
    }

    pub fn delete_worklog(&mut self, worklog: WorklogId) -> Option<Worklog> {
        let index = self.worklogs.iter().position(|w| w.id == worklog)?;
        Some(self.worklogs.remove(index))
    }

    /// Absorbs an activity into the day's primary task, or stops doing so. The choice
    /// carries over to every meeting in the same recurring series. Returns the activities
    /// that changed.
    pub fn set_absorbed(&mut self, activity: ActivityId, absorbed: bool) -> Vec<ActivityId> {
        let Some(series) = self.activity(activity).map(|a| a.series.clone()) else {
            return Vec::new();
        };
        match &series {
            Some(series) => self.series_absorbed.insert(series.clone(), absorbed),
            None => self.activity_absorbed.insert(activity, absorbed),
        };
        let mut changed = Vec::new();
        for a in &mut self.activities {
            let same = a.id == activity || (series.is_some() && a.series == series);
            if same && a.absorbed != absorbed {
                a.absorbed = absorbed;
                changed.push(a.id);
            }
        }
        changed
    }

    /// Replaces the activities from `source` that start inside `window` with a fresh load.
    ///
    /// An activity that was already here keeps its absorb choice, and worklogs keep
    /// pointing at it, as long as its id is stable. A new occurrence of a recurring series
    /// follows the choice made for the series. Otherwise, recurring meetings without an
    /// issue key start out absorbed: standups and similar meetings count as part of the
    /// primary task.
    pub fn replace_activities(&mut self, source: Source, window: TimeRange, incoming: Vec<Activity>) {
        let previous: HashMap<ActivityId, bool> = self
            .activities
            .iter()
            .filter(|a| a.source == source && window.contains(a.range.start))
            .map(|a| (a.id, a.absorbed))
            .collect();
        self.activities
            .retain(|a| !(a.source == source && window.contains(a.range.start)));
        for mut activity in incoming {
            activity.absorbed = previous
                .get(&activity.id)
                .or_else(|| self.activity_absorbed.get(&activity.id))
                .copied()
                .or_else(|| activity.series.as_ref().and_then(|s| self.series_absorbed.get(s).copied()))
                .unwrap_or_else(|| activity.series.is_some() && self.issue_hint(&activity).is_none());
            self.activities.push(activity);
        }
    }

    /// Issues to log `focus` on, best first, among the projects time may be logged on.
    pub fn suggestions(&self, focus: TimeRange, hint: Option<&IssueKey>) -> Vec<Suggestion> {
        let allowed: Vec<Issue>;
        let issues = if self.settings.projects.is_empty() {
            &self.issues[..]
        } else {
            allowed = self.issues.iter().filter(|i| self.settings.allows(&i.key)).cloned().collect();
            &allowed[..]
        };
        suggest::rank(&SuggestContext {
            issues,
            activities: &self.activities,
            worklogs: &self.worklogs,
            viewed: &self.viewed,
            history: &self.history,
            focus,
            hint,
            tz: &self.tz,
        })
    }

    /// The issue a day was most likely spent on, if anything points to one.
    pub fn primary_issue(&self, date: Date) -> Option<IssueKey> {
        let (h, m) = self.settings.day_start;
        let start = time::at(date, h, m, &self.tz);
        let workday = TimeRange::starting_at(start, self.settings.target);
        self.suggestions(workday, None)
            .into_iter()
            .find(|s| s.score > 0)
            .map(|s| s.key)
    }

    /// Meetings on `date` that name an issue in their title, aren't absorbed and aren't
    /// logged yet. Each of these is logged on the issue it names.
    pub fn named_meetings(&self, date: Date) -> Vec<(ActivityId, IssueKey, TimeRange)> {
        let day = self.day(date);
        self.activities_on(date)
            .filter(|a| a.source == Source::Outlook && !a.absorbed)
            .filter(|a| self.worklog_for(a.id).is_none())
            .filter_map(|a| {
                let range = self.day_parts(a).into_iter().find(|part| part.overlaps(&day))?;
                (range.duration() >= self.settings.min_block).then_some((a.id, self.issue_hint(a)?, range))
            })
            .collect()
    }

    /// What "fill day" would log on `date`: named meetings on their issues, then gaps on
    /// the primary task until the day reaches its target.
    pub fn fill_plan(&self, date: Date) -> FillPlan {
        let meetings = self.named_meetings(date);
        let gaps = self.gaps(date, meetings.iter().map(|(_, _, range)| *range));
        FillPlan { meetings, gaps }
    }

    fn gaps(&self, date: Date, also_taken: impl Iterator<Item = TimeRange>) -> Vec<TimeRange> {
        let day = self.day(date);
        let logged: Vec<TimeRange> = self
            .worklogs_on(date)
            .filter_map(|w| w.range.clip(&day))
            .chain(also_taken)
            .collect();
        let earliest = self.activities_on(date).map(|a| a.range.start.max(day.start)).min();
        let (h, m) = self.settings.day_start;
        fill::fill_gaps(&fill::FillRequest {
            day_start: fill::day_start(time::at(date, h, m, &self.tz), earliest),
            target: self.settings.target,
            logged: &logged,
            min_block: self.settings.min_block,
        })
    }

    /// Carries out [`Workspace::fill_plan`] with `issue` as the primary task. A new block
    /// that touches an unsynced worklog for the same issue extends it instead of adding
    /// another.
    pub fn fill_day(&mut self, date: Date, issue: IssueKey) -> Vec<WorklogId> {
        let gaps = self.fill_plan(date).gaps;
        let mut touched = self.log_named_meetings(date);
        touched.extend(self.log_ranges(&issue, &gaps));
        touched
    }

    /// Logs each of `date`'s meetings that name an issue on that issue.
    pub fn log_named_meetings(&mut self, date: Date) -> Vec<WorklogId> {
        let meetings = self.named_meetings(date);
        meetings.into_iter().filter_map(|(activity, issue, _)| self.link(activity, issue)).collect()
    }

    /// Logs each range on `issue`. A range that touches an unsynced worklog for the same
    /// issue extends it instead of adding another.
    pub fn log_ranges(&mut self, issue: &IssueKey, ranges: &[TimeRange]) -> Vec<WorklogId> {
        ranges.iter().map(|range| self.place(issue, *range)).collect()
    }

    /// Logs `range` on `issue`, extending an unsynced worklog for the same issue that it
    /// touches instead of adding another.
    fn place(&mut self, issue: &IssueKey, range: TimeRange) -> WorklogId {
        let neighbour = self.worklogs.iter_mut().find(|w| {
            w.remote_id.is_none()
                && w.origin.is_none()
                && &w.issue == issue
                && (w.range.end == range.start || w.range.start == range.end)
        });
        match neighbour {
            Some(w) => {
                w.range = TimeRange::new(w.range.start.min(range.start), w.range.end.max(range.end));
                w.id
            }
            None => self.add_worklog(issue.clone(), range, None),
        }
    }

    /// Hours logged on `date`, per issue, in the order each issue was first logged that day.
    pub fn logged_by_issue(&self, date: Date) -> Vec<(IssueKey, SignedDuration)> {
        let day = self.day(date);
        let mut worklogs: Vec<&Worklog> = self.worklogs_on(date).collect();
        worklogs.sort_by_key(|w| w.range.start);
        let mut totals: Vec<(IssueKey, SignedDuration)> = Vec::new();
        for w in worklogs {
            let duration = w.range.clip(&day).map_or(SignedDuration::ZERO, |r| r.duration());
            match totals.iter_mut().find(|(issue, _)| *issue == w.issue) {
                Some((_, total)) => *total += duration,
                None => totals.push((w.issue.clone(), duration)),
            }
        }
        totals
    }

    pub fn hours_on(&self, date: Date, issue: &IssueKey) -> SignedDuration {
        self.logged_by_issue(date)
            .into_iter()
            .find(|(i, _)| i == issue)
            .map_or(SignedDuration::ZERO, |(_, d)| d)
    }

    /// Makes `issue` add up to `target` on `date`, the way a timesheet cell edit should.
    /// Extra time goes into the day's free time from the start of the workday. Less time
    /// trims the most recent worklogs first, and those made for meetings last, so a
    /// meeting keeps its own worklog for as long as possible.
    pub fn set_hours(&mut self, date: Date, issue: &IssueKey, target: SignedDuration) {
        let target = target.max(SignedDuration::ZERO);
        let current = self.hours_on(date, issue);
        if target > current {
            self.add_time(date, issue, target - current);
        } else if target < current {
            self.remove_time(date, issue, current - target);
        }
    }

    fn add_time(&mut self, date: Date, issue: &IssueKey, amount: SignedDuration) {
        let day = self.day(date);
        let logged: Vec<TimeRange> = self.worklogs_on(date).filter_map(|w| w.range.clip(&day)).collect();
        let already: SignedDuration = logged.iter().map(|r| r.duration()).sum();
        let (h, m) = self.settings.day_start;
        let earliest = self.activities_on(date).map(|a| a.range.start.max(day.start)).min();
        let ranges = fill::fill_gaps(&fill::FillRequest {
            day_start: fill::day_start(time::at(date, h, m, &self.tz), earliest),
            target: already + amount,
            logged: &logged,
            min_block: self.settings.min_block.min(amount),
        });
        for range in ranges {
            self.place(issue, range);
        }
    }

    fn remove_time(&mut self, date: Date, issue: &IssueKey, amount: SignedDuration) {
        let mut candidates: Vec<(bool, jiff::Timestamp, WorklogId)> = self
            .worklogs_on(date)
            .filter(|w| &w.issue == issue)
            .map(|w| (w.origin.is_some(), w.range.start, w.id))
            .collect();
        // Plain worklogs before meeting ones, latest first within each.
        candidates.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let mut remaining = amount;
        for (_, _, id) in candidates {
            if remaining.is_zero() {
                break;
            }
            let Some(w) = self.worklogs.iter_mut().find(|w| w.id == id) else { continue };
            let duration = w.range.duration();
            if duration <= remaining {
                remaining -= duration;
                self.delete_worklog(id);
            } else {
                w.range.end -= remaining;
                remaining = SignedDuration::ZERO;
            }
        }
    }

    /// Moves everything logged on `from` on `date` to `to`.
    pub fn move_hours(&mut self, date: Date, from: &IssueKey, to: &IssueKey) {
        let day = self.day(date);
        for w in &mut self.worklogs {
            if &w.issue == from && w.range.overlaps(&day) {
                w.issue = to.clone();
            }
        }
    }

    pub fn sync_plan(&self) -> Vec<SyncOp> {
        sync::plan(&self.remote, &self.worklogs)
    }

    /// Records that `op` was written to Jira. `created` is the id Jira gave a new worklog.
    pub fn record_synced(&mut self, op: &SyncOp, created: Option<RemoteWorklogId>) {
        match op {
            SyncOp::Create(w) | SyncOp::Update { after: w, .. } | SyncOp::Move { after: w, .. } => {
                self.conflicts.remove(&w.id);
            }
            SyncOp::Delete(_) => {}
        }
        let to_remote = |w: &Worklog, id: RemoteWorklogId| RemoteWorklog {
            id,
            issue: w.issue.clone(),
            range: w.range,
            comment: w.comment.clone(),
        };
        match op {
            SyncOp::Create(after) | SyncOp::Move { after, .. } => {
                if let SyncOp::Move { before, .. } = op {
                    self.remote.retain(|r| r.id != before.id);
                }
                let id = created.expect("Jira returns an id for a created worklog");
                self.remote.push(to_remote(after, id.clone()));
                if let Some(w) = self.worklogs.iter_mut().find(|w| w.id == after.id) {
                    w.remote_id = Some(id);
                }
            }
            SyncOp::Update { before, after } => {
                if let Some(r) = self.remote.iter_mut().find(|r| r.id == before.id) {
                    *r = to_remote(after, before.id.clone());
                }
            }
            SyncOp::Delete(before) => self.remote.retain(|r| r.id != before.id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IssueKind, Source, StatusCategory, time::at};
    use jiff::civil::date;

    const DAY: Date = date(2026, 10, 27);

    fn key(s: &str) -> IssueKey {
        IssueKey::parse(s).unwrap()
    }

    fn range(from: (i8, i8), to: (i8, i8)) -> TimeRange {
        TimeRange::new(at(DAY, from.0, from.1, &TimeZone::UTC), at(DAY, to.0, to.1, &TimeZone::UTC))
    }

    fn workspace() -> Workspace {
        let mut ws = Workspace::new(TimeZone::UTC, Settings::default());
        ws.issues.push(Issue {
            key: key("AB-1"),
            summary: "Thing".into(),
            kind: IssueKind::Story,
            status: StatusCategory::InProgress,
            epic: None,
            business_unit: None,
            labels: Vec::new(),
            assigned_to_me: true,
        });
        for (id, title, series, from, to) in [
            (1, "Standup", Some("standup"), (9, 0), (9, 15)),
            (2, "Workshop AB-1", None, (12, 0), (14, 0)),
        ] {
            // Take ids from the workspace so later ones don't collide with these.
            assert_eq!(ws.next_id(), id);
            ws.activities.push(Activity {
                id: ActivityId(id),
                source: Source::Outlook,
                title: title.into(),
                range: range(from, to),
                series: series.map(Into::into),
                absorbed: series.is_some(),
            });
        }
        ws
    }

    #[test]
    fn linking_twice_moves_the_worklog_instead_of_doubling_it() {
        let mut ws = workspace();
        let first = ws.link(ActivityId(2), key("AB-1")).unwrap();
        let second = ws.link(ActivityId(2), key("BC-2")).unwrap();
        assert_eq!(first, second);
        assert_eq!(ws.worklogs.len(), 1);
        assert_eq!(ws.worklogs[0].issue, key("BC-2"));
    }

    #[test]
    fn linking_an_absorbed_meeting_stops_absorbing_it() {
        let mut ws = workspace();
        ws.link(ActivityId(1), key("AB-1"));
        assert!(!ws.activity(ActivityId(1)).unwrap().absorbed);
    }

    fn meeting(id: u64, title: &str, series: Option<&str>, from: (i8, i8), to: (i8, i8)) -> Activity {
        Activity {
            id: ActivityId(id),
            source: Source::Outlook,
            title: title.into(),
            range: range(from, to),
            series: series.map(Into::into),
            absorbed: false,
        }
    }

    #[test]
    fn only_allowed_projects_are_suggested_or_taken_from_titles() {
        let mut ws = workspace();
        ws.issues.push(Issue { key: key("SUP-55"), summary: "Support".into(), ..ws.issues[0].clone() });
        ws.activities.push(meeting(11, "SUP-55 support call", None, (15, 0), (16, 0)));
        ws.activities.push(meeting(12, "SUP-55 / AB-1 handover", None, (16, 0), (16, 30)));
        let day = ws.day(DAY);
        let suggested = |ws: &Workspace| ws.suggestions(day, None).into_iter().map(|s| s.key.to_string()).collect::<Vec<_>>();
        assert_eq!(suggested(&ws).len(), 2);

        ws.settings.projects = vec!["ab".into()];
        assert_eq!(suggested(&ws), ["AB-1"]);
        assert_eq!(ws.issue_hint(ws.activity(ActivityId(11)).unwrap()), None);
        assert_eq!(ws.issue_hint(ws.activity(ActivityId(12)).unwrap()), Some(key("AB-1")));
        let named: Vec<String> = ws.fill_plan(DAY).meetings.iter().map(|(_, k, _)| k.to_string()).collect();
        assert!(!named.contains(&"SUP-55".to_owned()));
    }

    #[test]
    fn logging_a_meeting_over_two_days_logs_each_day() {
        let mut ws = workspace();
        let tomorrow = range((9, 0), (16, 30)).shifted(SignedDuration::from_hours(24));
        let gathering = Activity {
            id: ActivityId(30),
            source: Source::Outlook,
            title: "Team gathering".into(),
            range: TimeRange::new(range((9, 0), (16, 30)).start, tomorrow.end),
            series: None,
            absorbed: false,
        };
        ws.activities.push(gathering);
        ws.link(ActivityId(30), key("AB-1"));
        let logged: Vec<TimeRange> = ws.worklogs.iter().filter(|w| w.origin == Some(ActivityId(30))).map(|w| w.range).collect();
        assert_eq!(logged, [range((9, 0), (16, 30)), tomorrow]);
        ws.link(ActivityId(30), key("BC-2"));
        assert!(ws.worklogs.iter().filter(|w| w.origin == Some(ActivityId(30))).all(|w| w.issue == key("BC-2")));
        assert_eq!(ws.worklogs.iter().filter(|w| w.origin == Some(ActivityId(30))).count(), 2);
    }

    #[test]
    fn a_toggl_entry_copied_from_a_meeting_shows_once() {
        let mut ws = workspace();
        let toggl = |id: u64, title: &str, from: (i8, i8), to: (i8, i8)| Activity {
            source: Source::Toggl,
            ..meeting(id, title, None, from, to)
        };
        ws.activities.clear();
        ws.activities.push(meeting(11, "Team standup", Some("standup"), (10, 15), (10, 45)));
        ws.activities.push(toggl(12, "team  standup", (10, 15), (10, 45)));
        ws.activities.push(toggl(13, "AB-1 review", (10, 0), (11, 30)));
        ws.activities.push(toggl(14, "Team standup", (14, 0), (14, 30)));
        let shown: Vec<u64> = ws.activities_on(DAY).map(|a| a.id.0).collect();
        assert_eq!(shown, [11, 13, 14]);

        // A copy of a cancelled meeting goes with it, whatever the cancellation says.
        let cancelled = meeting(15, "Planning", None, (14, 0), (14, 30));
        ws.activities.push(toggl(16, "Planning", (14, 0), (14, 30)));
        ws.replace_skipped(ws.day(DAY), vec![cancelled]);
        let shown: Vec<u64> = ws.activities_on(DAY).map(|a| a.id.0).collect();
        assert_eq!(shown, [11, 13, 14]);
    }

    #[test]
    fn a_fresh_load_replaces_meetings_in_its_window_only() {
        let mut ws = workspace();
        let window = ws.day(DAY);
        let tomorrow = meeting(9, "Tomorrow", None, (9, 0), (10, 0));
        let mut tomorrow = tomorrow;
        tomorrow.range = tomorrow.range.shifted(SignedDuration::from_hours(24));
        ws.activities.push(tomorrow);
        ws.replace_activities(Source::Outlook, window, vec![meeting(20, "Planning", None, (10, 0), (11, 0))]);
        let titles: Vec<&str> = ws.activities.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(titles, ["Tomorrow", "Planning"]);
    }

    #[test]
    fn recurring_meetings_without_a_key_start_absorbed() {
        let mut ws = workspace();
        let window = ws.day(DAY);
        ws.replace_activities(
            Source::Outlook,
            window,
            vec![
                meeting(20, "Standup", Some("s"), (9, 0), (9, 15)),
                meeting(21, "Sprint review AB-1", Some("r"), (13, 0), (14, 0)),
                meeting(22, "Workshop", None, (10, 0), (12, 0)),
            ],
        );
        let absorbed: Vec<bool> = ws.activities.iter().map(|a| a.absorbed).collect();
        assert_eq!(absorbed, [true, false, false]);
    }

    #[test]
    fn a_reload_keeps_choices_and_links() {
        let mut ws = workspace();
        let window = ws.day(DAY);
        let load = || vec![meeting(20, "Standup", Some("s"), (9, 0), (9, 15)), meeting(22, "Workshop", None, (10, 0), (12, 0))];
        ws.replace_activities(Source::Outlook, window, load());
        ws.set_absorbed(ActivityId(20), false);
        ws.set_absorbed(ActivityId(22), true);
        ws.replace_activities(Source::Outlook, window, load());
        let absorbed: Vec<bool> = ws.activities.iter().map(|a| a.absorbed).collect();
        assert_eq!(absorbed, [false, true]);

        // Next week's standup follows the series choice.
        let mut next = meeting(30, "Standup", Some("s"), (9, 0), (9, 15));
        next.range = next.range.shifted(SignedDuration::from_hours(24 * 7));
        let next_week = window.shifted(SignedDuration::from_hours(24 * 7));
        ws.replace_activities(Source::Outlook, next_week, vec![next]);
        assert!(!ws.activity(ActivityId(30)).unwrap().absorbed);
    }

    #[test]
    fn absorbing_applies_to_the_whole_series() {
        let mut ws = workspace();
        let mut tomorrow = ws.activities[0].clone();
        tomorrow.id = ActivityId(3);
        tomorrow.range = tomorrow.range.shifted(SignedDuration::from_hours(24));
        ws.activities.push(tomorrow);
        let changed = ws.set_absorbed(ActivityId(1), false);
        assert_eq!(changed, [ActivityId(1), ActivityId(3)]);
    }

    #[test]
    fn fill_day_steps_around_linked_meetings_and_reaches_the_target() {
        let mut ws = workspace();
        ws.link(ActivityId(2), key("AB-1"));
        ws.fill_day(DAY, key("AB-1"));
        assert_eq!(ws.logged_on(DAY), ws.settings.target);
        let mut ranges: Vec<_> = ws.worklogs.iter().map(|w| w.range).collect();
        ranges.sort();
        assert_eq!(ranges, [range((8, 0), (12, 0)), range((12, 0), (14, 0)), range((14, 0), (15, 30))]);
    }

    #[test]
    fn fill_day_logs_meetings_that_name_an_issue_on_that_issue() {
        let mut ws = workspace();
        let plan = ws.fill_plan(DAY);
        assert_eq!(plan.meetings, [(ActivityId(2), key("AB-1"), range((12, 0), (14, 0)))]);
        assert_eq!(plan.total(), ws.settings.target);

        ws.fill_day(DAY, key("CD-9"));
        assert_eq!(ws.worklog_for(ActivityId(2)).unwrap().issue, key("AB-1"));
        assert_eq!(ws.logged_on(DAY), ws.settings.target);
        assert!(ws.fill_plan(DAY).is_empty());
    }

    #[test]
    fn absorbed_and_unnamed_meetings_are_left_to_the_primary_task() {
        let mut ws = workspace();
        ws.activities[1].title = "Workshop".into();
        let plan = ws.fill_plan(DAY);
        assert!(plan.meetings.is_empty());
        assert_eq!(plan.gaps, [range((8, 0), (15, 30))]);
    }

    #[test]
    fn filling_next_to_a_new_block_of_the_same_issue_extends_it() {
        let mut ws = workspace();
        ws.activities[1].title = "Workshop".into();
        ws.add_worklog(key("AB-1"), range((10, 0), (11, 0)), None);
        ws.fill_day(DAY, key("AB-1"));
        assert_eq!(ws.worklogs.len(), 1);
        assert_eq!(ws.worklogs[0].range, range((8, 0), (15, 30)));
    }

    #[test]
    fn logging_a_meeting_takes_its_time_from_the_main_task() {
        let mut ws = workspace();
        ws.activities[1].title = "Workshop".into();
        ws.fill_day(DAY, key("AB-1"));
        assert_eq!(ws.logged_on(DAY), ws.settings.target);

        ws.link(ActivityId(2), key("BC-2")); // 12:00-14:00
        assert_eq!(ws.logged_on(DAY), ws.settings.target);
        assert_eq!(ws.hours_on(DAY, &key("BC-2")), SignedDuration::from_hours(2));
        let mut main: Vec<TimeRange> = ws.worklogs.iter().filter(|w| w.issue == key("AB-1")).map(|w| w.range).collect();
        main.sort();
        assert_eq!(main, [range((8, 0), (12, 0)), range((14, 0), (15, 30))]);
    }

    #[test]
    fn hours_are_totalled_per_issue_in_the_order_first_logged() {
        let mut ws = workspace();
        ws.add_worklog(key("BC-2"), range((13, 0), (14, 0)), None);
        ws.add_worklog(key("AB-1"), range((8, 0), (10, 0)), None);
        ws.add_worklog(key("BC-2"), range((15, 0), (15, 30)), None);
        let totals: Vec<(String, i64)> = ws
            .logged_by_issue(DAY)
            .into_iter()
            .map(|(k, d)| (k.to_string(), d.as_mins()))
            .collect();
        assert_eq!(totals, [("AB-1".into(), 120), ("BC-2".into(), 90)]);
    }

    #[test]
    fn raising_hours_fills_free_time_from_the_start_of_the_day() {
        let mut ws = workspace();
        ws.add_worklog(key("BC-2"), range((8, 0), (10, 0)), None);
        ws.set_hours(DAY, &key("AB-1"), SignedDuration::from_mins(150));
        assert_eq!(ws.hours_on(DAY, &key("AB-1")), SignedDuration::from_mins(150));
        let mine: Vec<TimeRange> = ws.worklogs.iter().filter(|w| w.issue == key("AB-1")).map(|w| w.range).collect();
        assert_eq!(mine, [range((10, 0), (12, 30))]);

        // Raising it again extends the same worklog rather than adding another.
        ws.set_hours(DAY, &key("AB-1"), SignedDuration::from_mins(180));
        let mine: Vec<TimeRange> = ws.worklogs.iter().filter(|w| w.issue == key("AB-1")).map(|w| w.range).collect();
        assert_eq!(mine, [range((10, 0), (13, 0))]);
    }

    #[test]
    fn lowering_hours_trims_plain_worklogs_before_meeting_ones() {
        let mut ws = workspace();
        ws.link(ActivityId(2), key("AB-1")); // 12:00-14:00
        ws.add_worklog(key("AB-1"), range((8, 0), (10, 0)), None);
        ws.set_hours(DAY, &key("AB-1"), SignedDuration::from_mins(150));
        assert_eq!(ws.hours_on(DAY, &key("AB-1")), SignedDuration::from_mins(150));
        assert_eq!(ws.worklog_for(ActivityId(2)).unwrap().range, range((12, 0), (14, 0)));

        ws.set_hours(DAY, &key("AB-1"), SignedDuration::from_mins(60));
        assert!(ws.worklogs.iter().all(|w| w.origin.is_some()));
        assert_eq!(ws.worklog_for(ActivityId(2)).unwrap().range, range((12, 0), (13, 0)));

        ws.set_hours(DAY, &key("AB-1"), SignedDuration::ZERO);
        assert!(ws.worklogs.is_empty());
    }

    #[test]
    fn moving_hours_changes_the_issue_of_that_day_only() {
        let mut ws = workspace();
        ws.add_worklog(key("AB-1"), range((8, 0), (10, 0)), None);
        let tomorrow = range((8, 0), (9, 0)).shifted(SignedDuration::from_hours(24));
        ws.add_worklog(key("AB-1"), tomorrow, None);
        ws.move_hours(DAY, &key("AB-1"), &key("BC-2"));
        assert_eq!(ws.hours_on(DAY, &key("BC-2")), SignedDuration::from_hours(2));
        assert_eq!(ws.hours_on(DAY, &key("AB-1")), SignedDuration::ZERO);
        assert!(ws.worklogs.iter().any(|w| w.issue == key("AB-1")));
    }

    fn remote(id: &str, issue: &str, from: (i8, i8), to: (i8, i8)) -> RemoteWorklog {
        RemoteWorklog { id: RemoteWorklogId(id.into()), issue: key(issue), range: range(from, to), comment: String::new() }
    }

    /// A workspace that pulled two worklogs from Jira.
    fn pulled() -> (Workspace, TimeRange) {
        let mut ws = workspace();
        let window = ws.day(DAY);
        ws.merge_pull(window, vec![remote("1", "AB-1", (8, 0), (10, 0)), remote("2", "AB-1", (10, 0), (12, 0))]);
        (ws, window)
    }

    fn local<'a>(ws: &'a Workspace, remote_id: &str) -> Option<&'a Worklog> {
        ws.worklogs.iter().find(|w| w.remote_id.as_ref().is_some_and(|r| r.0 == remote_id))
    }

    #[test]
    fn a_first_pull_brings_jira_worklogs_in_as_synced() {
        let (ws, _) = pulled();
        assert_eq!(ws.worklogs.len(), 2);
        assert!(ws.sync_plan().is_empty());
    }

    #[test]
    fn changes_made_in_jira_replace_untouched_worklogs() {
        let (mut ws, window) = pulled();
        ws.merge_pull(window, vec![remote("1", "AB-1", (8, 0), (11, 0))]);
        assert_eq!(local(&ws, "1").unwrap().range, range((8, 0), (11, 0)));
        assert!(local(&ws, "2").is_none(), "deleted in Jira");
        assert!(ws.sync_plan().is_empty());
    }

    #[test]
    fn edits_made_here_survive_a_pull() {
        let (mut ws, window) = pulled();
        let id = local(&ws, "1").unwrap().id;
        ws.set_range(id, range((8, 0), (9, 0)));
        ws.merge_pull(window, vec![remote("1", "AB-1", (8, 0), (10, 0)), remote("2", "AB-1", (10, 0), (12, 0))]);
        assert_eq!(ws.worklog(id).unwrap().range, range((8, 0), (9, 0)));
        assert!(ws.conflicts.is_empty());
        assert!(matches!(ws.sync_plan()[..], [SyncOp::Update { .. }]));
    }

    #[test]
    fn an_edit_here_that_jira_also_changed_is_a_conflict() {
        let (mut ws, window) = pulled();
        let id = local(&ws, "1").unwrap().id;
        ws.set_range(id, range((8, 0), (9, 0)));
        ws.merge_pull(window, vec![remote("1", "AB-1", (8, 0), (10, 30)), remote("2", "AB-1", (10, 30), (12, 0))]);
        assert_eq!(ws.worklog(id).unwrap().range, range((8, 0), (9, 0)));
        assert_eq!(ws.conflicts, HashSet::from([id]));
        let update = ws.sync_plan().remove(0);
        ws.record_synced(&update, None);
        assert!(ws.conflicts.is_empty());
    }

    #[test]
    fn an_edit_here_to_something_jira_deleted_is_created_again() {
        let (mut ws, window) = pulled();
        let id = local(&ws, "1").unwrap().id;
        ws.set_issue(id, key("BC-2"));
        ws.merge_pull(window, vec![remote("2", "AB-1", (10, 0), (12, 0))]);
        assert!(ws.worklog(id).unwrap().remote_id.is_none());
        assert!(matches!(ws.sync_plan()[..], [SyncOp::Create(_)]));
    }

    #[test]
    fn deleting_here_survives_a_pull_until_synced() {
        let (mut ws, window) = pulled();
        let id = local(&ws, "1").unwrap().id;
        ws.delete_worklog(id);
        ws.merge_pull(window, vec![remote("1", "AB-1", (8, 0), (10, 0)), remote("2", "AB-1", (10, 0), (12, 0))]);
        assert!(local(&ws, "1").is_none());
        assert!(matches!(ws.sync_plan()[..], [SyncOp::Delete(_)]));
    }

    #[test]
    fn each_pending_change_can_be_undone_on_its_own() {
        let (mut ws, _) = pulled();
        let first = local(&ws, "1").unwrap().id;
        let second = local(&ws, "2").unwrap().id;
        ws.set_range(first, range((8, 0), (11, 0)));
        ws.set_issue(first, key("AB-2"));
        ws.delete_worklog(second);
        ws.add_worklog(key("AB-1"), range((13, 0), (14, 0)), None);
        assert_eq!(ws.sync_plan().len(), 3);
        assert!(ws.jira_copy(ws.worklog(first).unwrap()).is_some());

        while let Some(op) = ws.sync_plan().first().cloned() {
            ws.revert(&op);
        }
        assert!(ws.sync_plan().is_empty());
        let moved_back = ws.worklog(first).unwrap();
        assert_eq!((moved_back.issue.as_str(), moved_back.range), ("AB-1", range((8, 0), (10, 0))));
        assert!(ws.jira_copy(moved_back).is_none());
        assert!(local(&ws, "2").is_some(), "the deleted worklog is back");
        assert_eq!(ws.worklogs.len(), 2, "the new worklog is gone");
    }

    #[test]
    fn a_pull_only_touches_its_window() {
        let (mut ws, _) = pulled();
        let tomorrow = ws.day(DAY).shifted(SignedDuration::from_hours(24));
        ws.merge_pull(tomorrow, Vec::new());
        assert_eq!(ws.worklogs.len(), 2);
    }

    #[test]
    fn saved_state_round_trips() {
        let (mut ws, _) = pulled();
        ws.set_absorbed(ActivityId(2), true);
        let json = serde_json::to_string(&ws.save()).unwrap();
        let mut restored = Workspace::new(TimeZone::UTC, Settings::default());
        restored.restore(serde_json::from_str(&json).unwrap());
        assert_eq!(restored.worklogs, ws.worklogs);
        assert_eq!(restored.remote, ws.remote);
        // A one-off meeting's choice applies when it's loaded again.
        let meeting = ws.activities[1].clone();
        restored.replace_activities(Source::Outlook, ws.day(DAY), vec![Activity { absorbed: false, ..meeting }]);
        assert!(restored.activity(ActivityId(2)).unwrap().absorbed);
    }

    #[test]
    fn a_synced_create_gets_its_remote_id_and_leaves_nothing_to_do() {
        let mut ws = workspace();
        ws.add_worklog(key("AB-1"), range((8, 0), (9, 0)), None);
        let plan = ws.sync_plan();
        assert_eq!(plan.len(), 1);
        ws.record_synced(&plan[0], Some(RemoteWorklogId("r1".into())));
        assert!(ws.sync_plan().is_empty());
        assert_eq!(ws.sync_state(&ws.worklogs[0]), SyncState::Synced);
    }

    #[test]
    fn a_synced_move_replaces_the_old_remote_worklog() {
        let mut ws = workspace();
        ws.add_worklog(key("AB-1"), range((8, 0), (9, 0)), None);
        let create = ws.sync_plan().remove(0);
        ws.record_synced(&create, Some(RemoteWorklogId("r1".into())));
        let id = ws.worklogs[0].id;
        ws.set_issue(id, key("BC-2"));
        let mv = ws.sync_plan().remove(0);
        assert!(matches!(mv, SyncOp::Move { .. }));
        ws.record_synced(&mv, Some(RemoteWorklogId("r2".into())));
        assert_eq!(ws.remote.len(), 1);
        assert_eq!(ws.remote[0].issue, key("BC-2"));
        assert!(ws.sync_plan().is_empty());
    }

    #[test]
    fn deleted_worklogs_stay_visible_until_synced() {
        let mut ws = workspace();
        let id = ws.add_worklog(key("AB-1"), range((8, 0), (9, 0)), None);
        let create = ws.sync_plan().remove(0);
        ws.record_synced(&create, Some(RemoteWorklogId("r1".into())));
        ws.delete_worklog(id);
        assert_eq!(ws.deleted_on(DAY).len(), 1);
        let delete = ws.sync_plan().remove(0);
        ws.record_synced(&delete, None);
        assert!(ws.deleted_on(DAY).is_empty());
    }
}
