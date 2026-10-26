//! Jira Cloud: your issues, your worklogs, and writing reviewed changes back.
//!
//! Authentication is your email and an API token. Searches use the token-paged
//! `/rest/api/3/search/jql` endpoint, which replaced the removed `/search`.

use backfill_core::{
    Issue, IssueKey, IssueKind, RemoteWorklog, RemoteWorklogId, StatusCategory, TimeRange,
    Worklog, sync::SyncOp,
};
use backfill_http::{Method, Request, Response, Transport, basic_auth};
use jiff::{Timestamp, civil::Date, tz::TimeZone};
use serde::Deserialize;
use serde_json::{Value, json};

/// How to reach a Jira site.
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    /// The site, such as `example.atlassian.net`, with or without `https://`.
    pub site: String,
    pub email: String,
    pub token: String,
}

/// Who the account is.
#[derive(Clone, Debug, PartialEq)]
pub struct Me {
    pub account_id: String,
    pub name: String,
}

/// The ids of the custom fields that say which business units an issue serves. Found by
/// the names set in Settings, since ids differ between sites.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fields {
    /// The issue's own business unit: half the logged hours are credited to it.
    pub business_unit: Option<String>,
    /// More business units: the other half is shared by them.
    pub labels: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    BadCredentials,
    Forbidden(String),
    NotFound(String),
    RateLimited { retry_after: Option<u64> },
    Unreachable(String),
    Unexpected { status: u16, message: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadCredentials => f.write_str("Jira didn't accept the email and API token"),
            Error::Forbidden(message) => write!(f, "Jira refused: {message}"),
            Error::NotFound(message) => write!(f, "Jira couldn't find it: {message}"),
            Error::RateLimited { .. } => f.write_str("Jira asked to slow down; try again in a minute"),
            Error::Unreachable(reason) => write!(f, "couldn't reach Jira: {reason}"),
            Error::Unexpected { status, message } => write!(f, "Jira answered {status}: {message}"),
        }
    }
}

pub struct Client<'a> {
    transport: &'a dyn Transport,
    base: String,
    auth: String,
}

const ISSUE_FIELDS: &[&str] = &["summary", "issuetype", "status", "parent", "assignee"];

impl<'a> Client<'a> {
    pub fn new(transport: &'a dyn Transport, account: &Account) -> Self {
        let site = account.site.trim().trim_end_matches('/');
        let base = if site.starts_with("http") { site.to_owned() } else { format!("https://{site}") };
        Self { transport, base, auth: basic_auth(account.email.trim(), account.token.trim()) }
    }

    fn send(&self, method: Method, path: &str, body: Option<Value>) -> Result<Response, Error> {
        let mut request = Request::new(method, format!("{}{path}", self.base))
            .header("Authorization", self.auth.clone())
            .header("Accept", "application/json");
        if let Some(body) = body {
            request = request.json(body.to_string());
        }
        let response = self.transport.send(request).map_err(Error::Unreachable)?;
        match response.status {
            200..=299 => Ok(response),
            401 => Err(Error::BadCredentials),
            403 => Err(Error::Forbidden(message(&response))),
            404 => Err(Error::NotFound(message(&response))),
            429 => Err(Error::RateLimited { retry_after: response.retry_after }),
            status => Err(Error::Unexpected { status, message: message(&response) }),
        }
    }

    fn json<T: for<'de> Deserialize<'de>>(&self, method: Method, path: &str, body: Option<Value>) -> Result<T, Error> {
        let response = self.send(method, path, body)?;
        serde_json::from_str(&response.body).map_err(|e| Error::Unexpected { status: response.status, message: e.to_string() })
    }

    pub fn me(&self) -> Result<Me, Error> {
        let me: MeJson = self.json(Method::Get, "/rest/api/3/myself", None)?;
        Ok(Me { account_id: me.account_id, name: me.display_name.unwrap_or_default() })
    }

    /// Finds the business unit fields by their names. Asks Jira nothing when no name is set.
    pub fn fields(&self, business_unit: &str, labels: &str) -> Result<Fields, Error> {
        let (business_unit, labels) = (business_unit.trim(), labels.trim());
        if business_unit.is_empty() && labels.is_empty() {
            return Ok(Fields::default());
        }
        let all: Vec<FieldJson> = self.json(Method::Get, "/rest/api/3/field", None)?;
        let find = |name: &str| {
            all.iter().find(|f| !name.is_empty() && f.custom && f.name.eq_ignore_ascii_case(name)).map(|f| f.id.clone())
        };
        Ok(Fields { business_unit: find(business_unit), labels: find(labels) })
    }

    /// Issues matching `jql`, up to `limit`, in the order Jira returns them.
    pub fn search(&self, jql: &str, fields: &Fields, me: &Me, limit: usize) -> Result<Vec<Issue>, Error> {
        let mut wanted: Vec<&str> = ISSUE_FIELDS.to_vec();
        wanted.extend(fields.business_unit.as_deref());
        wanted.extend(fields.labels.as_deref());
        let mut issues = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let body = json!({
                "jql": jql,
                "fields": wanted,
                "maxResults": (limit - issues.len()).min(100),
                "nextPageToken": page_token,
            });
            let page: SearchJson = self.json(Method::Post, "/rest/api/3/search/jql", Some(body))?;
            let empty = page.issues.is_empty();
            issues.extend(page.issues.into_iter().filter_map(|i| i.into_issue(fields, me)));
            page_token = page.next_page_token;
            if empty || page_token.is_none() || page.is_last.unwrap_or(false) || issues.len() >= limit {
                break;
            }
        }
        issues.truncate(limit);
        Ok(issues)
    }

    /// Issues by key, for summaries of keys found in meetings and commits. Unknown keys
    /// are left out.
    pub fn issues(&self, keys: &[IssueKey], fields: &Fields, me: &Me) -> Result<Vec<Issue>, Error> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let list = keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ");
        // A JQL search fails as a whole on a key that doesn't exist, so fall back to
        // one search per key if that happens.
        match self.search(&format!("key in ({list})"), fields, me, keys.len()) {
            Ok(issues) => Ok(issues),
            Err(Error::Unexpected { status: 400, .. }) => Ok(keys
                .iter()
                .filter_map(|k| self.search(&format!("key = {k}"), fields, me, 1).ok())
                .flatten()
                .collect()),
            Err(error) => Err(error),
        }
    }

    /// Your worklogs that start inside `window`.
    pub fn worklogs(&self, me: &Me, window: TimeRange, tz: &TimeZone) -> Result<Vec<RemoteWorklog>, Error> {
        let date = |ts: Timestamp| ts.to_zoned(tz.clone()).date();
        // A day either side, since Jira's worklogDate is in the site's time zone.
        let jql = format!(
            "worklogAuthor = currentUser() AND worklogDate >= \"{}\" AND worklogDate <= \"{}\"",
            shift(date(window.start), -1),
            shift(date(window.end), 1)
        );
        let keys: Vec<IssueKey> = self.search(&jql, &Fields::default(), me, 500)?.into_iter().map(|i| i.key).collect();
        let mut worklogs = Vec::new();
        for key in keys {
            let mut start_at = 0;
            loop {
                let path = format!(
                    "/rest/api/3/issue/{key}/worklog?startedAfter={}&startedBefore={}&startAt={start_at}&maxResults=1000",
                    window.start.as_millisecond(),
                    window.end.as_millisecond()
                );
                let page: WorklogPageJson = self.json(Method::Get, &path, None)?;
                let count = page.worklogs.len();
                worklogs.extend(
                    page.worklogs
                        .into_iter()
                        .filter(|w| w.author.as_ref().is_some_and(|a| a.account_id == me.account_id))
                        .filter_map(|w| w.into_remote(&key))
                        .filter(|w| window.contains(w.range.start)),
                );
                start_at += count;
                if count == 0 || start_at >= page.total.unwrap_or(0) {
                    break;
                }
            }
        }
        Ok(worklogs)
    }

    /// Writes one reviewed change. Returns the new worklog's Jira id for a create or move.
    /// A move creates the new worklog before deleting the old one, so a failure halfway
    /// never loses time.
    ///
    /// New time lowers the issue's remaining estimate, as logging work in Jira does.
    /// Changing or removing a worklog leaves the estimate alone: Jira would otherwise add
    /// the difference back, and on an issue without an estimate that makes one up, so
    /// shortening a 1.7h worklog to 1.2h would leave "0.5h remaining".
    pub fn apply(&self, op: &SyncOp, tz: &TimeZone) -> Result<Option<RemoteWorklogId>, Error> {
        match op {
            SyncOp::Create(after) => self.create(after, tz).map(Some),
            SyncOp::Update { before, after } => {
                let path = format!("/rest/api/3/issue/{}/worklog/{}?notifyUsers=false&adjustEstimate=leave", after.issue, before.id.0);
                self.send(Method::Put, &path, Some(body(after, tz, after.comment != before.comment)))?;
                Ok(None)
            }
            SyncOp::Move { before, after } => {
                let created = self.create(after, tz)?;
                self.delete(before)?;
                Ok(Some(created))
            }
            SyncOp::Delete(before) => {
                self.delete(before)?;
                Ok(None)
            }
        }
    }

    fn create(&self, worklog: &Worklog, tz: &TimeZone) -> Result<RemoteWorklogId, Error> {
        let path = format!("/rest/api/3/issue/{}/worklog?notifyUsers=false&adjustEstimate=auto", worklog.issue);
        let created: CreatedJson = self.json(Method::Post, &path, Some(body(worklog, tz, !worklog.comment.is_empty())))?;
        Ok(RemoteWorklogId(created.id))
    }

    fn delete(&self, worklog: &RemoteWorklog) -> Result<(), Error> {
        let path = format!("/rest/api/3/issue/{}/worklog/{}?notifyUsers=false&adjustEstimate=leave", worklog.issue, worklog.id.0);
        match self.send(Method::Delete, &path, None) {
            // Already gone is what we wanted.
            Ok(_) | Err(Error::NotFound(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// A worklog's JSON for Jira. `started` carries the local offset, as Jira expects.
fn body(worklog: &Worklog, tz: &TimeZone, with_comment: bool) -> Value {
    let started = worklog.range.start.to_zoned(tz.clone()).strftime("%Y-%m-%dT%H:%M:%S%.3f%z").to_string();
    let mut body = json!({
        "started": started,
        "timeSpentSeconds": worklog.range.duration().as_secs().max(60),
    });
    if with_comment {
        body["comment"] = json!({
            "type": "doc",
            "version": 1,
            "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": worklog.comment }] }],
        });
    }
    body
}

fn shift(date: Date, days: i64) -> Date {
    date.checked_add(jiff::Span::new().days(days)).expect("date in range")
}

/// Jira's own words for what went wrong, if it gave any.
fn message(response: &Response) -> String {
    let parsed: Option<ErrorJson> = serde_json::from_str(&response.body).ok();
    let mut parts: Vec<String> = Vec::new();
    if let Some(error) = parsed {
        parts.extend(error.error_messages);
        parts.extend(error.errors.into_iter().map(|(field, message)| match message.as_str() {
            Some(text) => format!("{field}: {text}"),
            None => format!("{field}: {message}"),
        }));
    }
    if parts.is_empty() {
        response.body.chars().take(200).collect()
    } else {
        parts.join("; ")
    }
}

/// The plain text of an Atlassian Document Format comment.
fn plain_text(doc: &Value) -> String {
    fn walk(node: &Value, out: &mut String) {
        if let Some(text) = node.get("text").and_then(Value::as_str) {
            out.push_str(text);
        }
        if let Some(children) = node.get("content").and_then(Value::as_array) {
            for child in children {
                walk(child, out);
            }
            if node.get("type").and_then(Value::as_str) == Some("paragraph") {
                out.push('\n');
            }
        }
    }
    let mut out = String::new();
    walk(doc, &mut out);
    out.trim().to_owned()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MeJson {
    account_id: String,
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct FieldJson {
    id: String,
    name: String,
    #[serde(default)]
    custom: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchJson {
    #[serde(default)]
    issues: Vec<IssueJson>,
    next_page_token: Option<String>,
    is_last: Option<bool>,
}

#[derive(Deserialize)]
struct IssueJson {
    key: String,
    #[serde(default)]
    fields: serde_json::Map<String, Value>,
}

impl IssueJson {
    fn into_issue(self, custom: &Fields, me: &Me) -> Option<Issue> {
        let field = |name: &str| self.fields.get(name).filter(|v| !v.is_null());
        let text = |value: &Value| {
            value.as_str().map(str::to_owned).or_else(|| value.get("value").and_then(Value::as_str).map(str::to_owned))
        };
        let type_name = field("issuetype").and_then(|t| t.get("name")).and_then(Value::as_str).unwrap_or("");
        let subtask = field("issuetype").and_then(|t| t.get("subtask")).and_then(Value::as_bool).unwrap_or(false);
        let kind = match type_name.to_lowercase().as_str() {
            "epic" => IssueKind::Epic,
            "story" => IssueKind::Story,
            "task" => IssueKind::Task,
            "bug" => IssueKind::Bug,
            "work log" | "worklog" => IssueKind::WorkLog,
            _ if subtask => IssueKind::SubTask,
            _ => IssueKind::Other,
        };
        let status = match field("status").and_then(|s| s.pointer("/statusCategory/key")).and_then(Value::as_str) {
            Some("done") => StatusCategory::Done,
            Some("indeterminate") => StatusCategory::InProgress,
            _ => StatusCategory::ToDo,
        };
        let parent = field("parent");
        let parent_is_epic = parent
            .and_then(|p| p.pointer("/fields/issuetype/name"))
            .and_then(Value::as_str)
            .is_some_and(|name| name.eq_ignore_ascii_case("epic"));
        let epic = parent
            .filter(|_| parent_is_epic)
            .and_then(|p| p.get("key"))
            .and_then(Value::as_str)
            .and_then(IssueKey::parse);
        let business_unit = custom.business_unit.as_deref().and_then(field).and_then(text);
        let labels = custom
            .labels
            .as_deref()
            .and_then(field)
            .map(|v| match v.as_array() {
                Some(items) => items.iter().filter_map(text).collect(),
                None => text(v).into_iter().collect(),
            })
            .unwrap_or_default();
        let assigned_to_me = field("assignee")
            .and_then(|a| a.get("accountId"))
            .and_then(Value::as_str)
            .is_some_and(|id| id == me.account_id);
        Some(Issue {
            key: IssueKey::parse(&self.key)?,
            summary: field("summary").and_then(Value::as_str).unwrap_or_default().to_owned(),
            kind,
            status,
            epic,
            business_unit,
            labels,
            assigned_to_me,
        })
    }
}

#[derive(Deserialize)]
struct WorklogPageJson {
    #[serde(default)]
    worklogs: Vec<WorklogJson>,
    total: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorklogJson {
    id: String,
    author: Option<AuthorJson>,
    started: String,
    time_spent_seconds: i64,
    comment: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorJson {
    account_id: String,
}

impl WorklogJson {
    fn into_remote(self, issue: &IssueKey) -> Option<RemoteWorklog> {
        let start = jiff::fmt::strtime::parse("%Y-%m-%dT%H:%M:%S%.f%z", &self.started).ok()?.to_timestamp().ok()?;
        Some(RemoteWorklog {
            id: RemoteWorklogId(self.id),
            issue: issue.clone(),
            range: TimeRange::starting_at(start, jiff::SignedDuration::from_secs(self.time_spent_seconds)),
            comment: self.comment.as_ref().map(plain_text).unwrap_or_default(),
        })
    }
}

#[derive(Deserialize)]
struct CreatedJson {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ErrorJson {
    #[serde(default)]
    error_messages: Vec<String>,
    #[serde(default)]
    errors: serde_json::Map<String, Value>,
}

#[cfg(test)]
mod tests;
