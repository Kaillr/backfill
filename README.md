# backfill

A week calendar for getting your hours into Jira. It shows your Outlook meetings and Toggl
timers next to the time you've logged, helps you fill the gaps on the right issues, and
writes nothing to Jira until you've reviewed exactly what will change.

## Run it

```sh
cargo run --release
```

On macOS, `cargo run` opens backfill as an app (see `scripts/macos-runner.sh`), because
macOS only lets apps ask for calendar access. Output still shows in the terminal, and Ctrl-C
quits.

The first time, Settings opens. Add your Jira site, email and an
[API token](https://id.atlassian.com/manage-profile/security/api-tokens), and optionally your
[Toggl API token](https://track.toggl.com/profile). macOS asks once whether backfill may read
your calendars.

macOS needs Xcode (for Metal). On Windows and Linux, see
[gpui-ce's setup notes](https://github.com/gpui-ce/gpui-ce/blob/main/SETUP.md). Outlook
meetings aren't available there yet.

## Where things are kept

- **Tokens** are in the macOS Keychain, under "backfill". They're only sent to Jira and Toggl.
- **Settings and caches** are JSON files in `~/Library/Application Support/backfill`:
  `settings.json`, `state.json` (your worklogs, edits not yet synced, and choices like which
  meetings are part of the main task) and `toggl.json`. Set `BACKFILL_DATA_DIR` to use another
  folder for trying things out. That run doesn't read or write the Keychain, and tokens
  entered in it are forgotten when backfill quits.

## Jira

backfill reads your worklogs for the visible week and the weeks either side, your assigned
issues, and the ones you viewed recently. For each of those weeks it also asks which issues
were assigned to you then and which you moved to another status, so backfilling an old week
suggests what you were on that week rather than what's in progress today. The issue search
looks up anything you type in all of Jira, including issues you've never opened, and says
while it's searching. The sync review opens at once and reads the affected days from Jira
again while you look. Sync stays off until that's done, so if something was changed in Jira
meanwhile, the review says so instead of overwriting it unseen. If Jira can't be reached,
the review says why and Sync becomes Try again.

If your team logs everything on certain projects, list them in Settings under "Log time only
on these projects", such as `APP`. Suggestions, the issue search and Fill then stay inside
those projects. Typing another project's full key, like an event's issue someone asked you to
log on, still finds it, marked "Outside your projects". A title that names another project's
issue, like "SUP-55 support", is treated as if it named none, so that time goes on your main
task. Worklogs already in Jira on other
projects still show, and nothing is changed on them.

If your Jira records which business units an issue serves, name those two custom fields under
"Business units" in Settings. The footer then shows the week's hours credited to each unit:
half of an issue's hours go to its business unit, the rest split evenly across its shared
units. With the fields left empty, backfill skips this and asks Jira nothing about them.

Each change in the review has an Undo, which drops just that change: a new block goes, a
changed one goes back to what Jira has, and a removed one comes back. `⌘ Z` brings it back
again. New time lowers the issue's remaining estimate, as logging work in Jira does. Changing or
removing a worklog leaves the estimate alone, so backfill never adds remaining time that
nobody estimated. Syncing writes one change at a time. A move to another issue creates the new worklog before
deleting the old one, so a failure halfway never loses time. A change that fails stays
waiting for the next sync, and the rest still go through.

## Toggl

Toggl allows 30 requests an hour for your own data, and only reaches back about three
months. So backfill keeps every entry from those three months on disk. It fills the cache
with one request, then keeps it current with one request for whatever changed since last
time. Moving between weeks never asks Toggl. Syncs are at least 10 minutes apart unless you
press `⌘ R`, stay under 24 requests an hour, and pause for as long as Toggl asks when it
says to slow down.

## Git

No GitHub account or token is needed. backfill reads the repositories already on your
computer, in the folders set in Settings (`~/git` by default, one level deep). It only counts
your own commits, by the email git commits with, and the branches you switch to. To keep
it to work, list your GitHub organisations, such as `acme`. Git doesn't know how long you
worked, so it draws nothing. It ranks the issues you probably worked on in the search, and
Fill uses it to pick the day's main task.

## Outlook meetings

Microsoft offers no personal API key for Outlook, and the other routes need IT to approve
an app. The macOS Calendar app already syncs your Outlook account, so backfill reads the
meetings from there, locally:

- It uses your Exchange calendars, or every calendar except birthdays and subscriptions
  if there's no Exchange account.
- It skips all-day events, events shown as free, meetings you declined, and cancelled ones.
  The Calendar app can mark a meeting cancelled, but Exchange often only renames it in the
  organizer's language ("Canceled:", "Avlyst:", "Avbruten:", and so on), so those titles
  count as cancelled too. A Toggl entry made from a cancelled or declined meeting is skipped
  along with it.
- Recurring meetings without an issue key in the title start out absorbed into the day's
  main task. Press `A` to change that for the whole series.
- It rereads the calendar when you change week, when you switch back to the window, and
  every two minutes.
- `target/backfill.log` gets counts only, never meeting titles.

Calendar on macOS syncs Exchange through Exchange Web Services, which Microsoft is
retiring: blocked by default since 1 October 2026 unless an organisation keeps it on,
and gone on 1 April 2027. Apple has said a macOS 27 update moves Calendar onto Microsoft
Graph before then.

Signing: the app is signed with your "Apple Development" identity if you have one, so
macOS remembers the calendar permission across rebuilds. Otherwise it's signed ad hoc and
macOS may ask again after a rebuild. `BACKFILL_SIGN_IDENTITY` picks another identity, or
`-` for ad hoc.

## How it reads

It's a week calendar, like Outlook's.

- **Solid blocks** are logged time, in each issue's colour, as Jira has it.
- **Dashed, lighter blocks** marked "New" aren't in Jira yet.
- **Blocks with an amber edge** were changed here. Their badge says how: "+30m", "1h
  later", or "from APP-1" when moved to another issue. A faint dashed outline shows where
  Jira still has the time.
- **Red dashed outlines** with the text struck through were removed here and are deleted
  from Jira when you sync.
- **Grey blocks** are Outlook meetings and Toggl timers that still need a decision. Hover
  one for its Log button. When the title names an issue (`Review APP-123`), the button logs
  it there in one click. Recurring meetings without an issue key are faded: they count as
  part of the day's main task. Once logged time covers a
  meeting, it's accounted for and disappears. Events of 6 hours or more, and meetings over
  several days, sit under the day headings like Outlook's all-day events, so they never
  cover the day. One over several days is a single bar across them, with its own start and
  end time. Logging it logs each weekday separately, and the issue search says which hours. One that sets
  its daily hours, like a gathering from 09:00 on Wednesday to 16:30 on Thursday, is those
  hours on each day. One that runs on into the next day, like a hackathon from Thursday
  09:00 to Friday 13:00, is your working day on each: from 09:00 to the end of Thursday's,
  and from the start of Friday's to 13:00.
- **Day headings** show hours logged out of 7.5. Double-click a heading, press `F`, or
  right-click it to fill the day.
- **Blue outlined blocks** are drafts: time waiting for an issue. Drawing makes one, and
  Fill drafts the rest of the day. While the issue search is open, drag a draft to move it
  or its top and bottom edges to resize it, then pick the issue. Filling also logs the day's
  meetings that name an issue on that issue. Esc or a click elsewhere drops the drafts.

Overlapping blocks sit side by side, like Outlook, and make room for a draft while it's
still being drawn. Dragging always draws new time, even when it starts on a meeting. Logging a meeting takes its time out of the block it overlaps,
so the day still adds up. The calendar scrolls through the whole day and opens on 07:00 to
17:00.

Suggestions are weighed against the time being logged, not today. In rough order of weight:
an issue the title names, commits made then, the branch you had checked out then (from
switching to it until switching away), commits that day, an issue named by a Toggl entry or
meeting that day, what Jira says you moved or were assigned that week, time logged that day
or nearby, and issues you viewed. Each suggestion says why it's there, such as "3 commits that
day" or "you moved it that week".

## Keys

| Key | Does |
| --- | --- |
| `N` | Select the next thing that needs a decision |
| `F` | Fill the day: draft the rest of it, then pick the issue |
| `L` or `Enter` | Log the selected meeting, or move the selected block to another issue |
| `A` | Make a meeting part of the main task, or log it on its own (applies to the whole series) |
| `Delete` | Remove the selected block |
| `⌘ O` | Open the selected block's issue in Jira, or the highlighted one in the issue search |
| `←` `→` | Previous or next week |
| `⌘ Z` | Undo |
| `⌘ Enter` | Review what a sync would change in Jira. Press it again in the review to sync |
| `⌘ R` | Sync Jira, Toggl, git and Outlook now |
| `⌘ ,` | Settings |

Right-click a block, or Control-click it, for the same actions in a menu. The menu shows each
action's key, and works with the arrow keys and Enter.

Single-key shortcuts never fire while you're typing in the issue search.

With the mouse: drag on empty space to log time, drag a block to move it, drag its top or
bottom edge to resize it, and double-click anything to pick its issue.

## Layout

- `crates/mac-calendar`: reads events through EventKit. It compiles to a stub on other
  platforms.
- `crates/jira`, `crates/toggl`, `crates/git`: the three other sources. `crates/http` is the
  small HTTP layer they share, with a fake transport for tests.
- `crates/core`: the domain, with no UI. Activities, worklogs, the fill rules, issue
  suggestions, the sync plan, merging what Jira has with edits made here, and
  business-unit attribution. Test data is behind its `fixtures` feature. Run its tests
  with `cargo test -p backfill-core`.
- `crates/app`: the gpui-ce window. `calendar.rs` is the week view's state and actions,
  `calendar/week.rs` draws it, and `calendar/review.rs` is the sync review. `picker.rs` is
  the issue search, and `motion.rs` the spring-animated geometry.
- `vendor/gpui_ce_macos`: gpui-ce 0.3.0's macOS backend with one fix. The published crate
  deadlocks when an app opens a window at launch. gpui-ce's `main` already has the fix, so
  delete this and the `[patch]` in `Cargo.toml` once a release includes it.

## Development

`cargo test --workspace` runs the domain tests and headless UI tests. Those drive the real
key bindings and pointer events.

Debug builds also accept `BACKFILL_SCRIPT` to replay steps after launch, which is handy
for checking a state without clicking to it:

```sh
BACKFILL_SCRIPT="n f wait sync" cargo run
```

Steps: `n f l a del left right sync apply enter esc undo settings wait`, plus `type:TEXT`,
`up` and `down` while the issue search is open, `key:KEYS`, `text:TEXT`, `click:X,Y`,
`rclick:X,Y` and `drag:X1,Y1,X2,Y2` for real input, and `shot:NAME`, which runs
`$BACKFILL_SHOT NAME PID`. `BACKFILL_THEME=light` or `dark` overrides the
system appearance.

## Next

1. Read Outlook through Microsoft Graph, with read-only access to your own calendar,
   instead of through the Mac calendar. It also works on Windows and says outright which
   meetings are cancelled.
2. Start and stop Toggl timers from backfill.
3. Choose which calendars count as Outlook, in Settings.

## License

MIT, see `LICENSE`. The patched gpui-ce crates in `vendor/` keep their Apache-2.0 licence;
see `vendor/README.md`.
