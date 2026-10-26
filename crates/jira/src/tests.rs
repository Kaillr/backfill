use backfill_http::fake::Fake;
use jiff::SignedDuration;

use super::*;

fn account() -> Account {
    Account { site: "example.atlassian.net/".into(), email: "me@example.com".into(), token: "secret".into() }
}

fn me() -> Me {
    Me { account_id: "acc-me".into(), name: "Me".into() }
}

fn oslo() -> TimeZone {
    TimeZone::get("Europe/Oslo").unwrap()
}

fn key(s: &str) -> IssueKey {
    IssueKey::parse(s).unwrap()
}

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

const ISSUE: &str = r#"{
    "key": "APP-123",
    "fields": {
        "summary": "Fix the export button",
        "issuetype": {"name": "Story", "subtask": false},
        "status": {"statusCategory": {"key": "indeterminate"}},
        "parent": {"key": "APP-100", "fields": {"issuetype": {"name": "Epic"}}},
        "assignee": {"accountId": "acc-me"},
        "customfield_10100": {"value": "North"},
        "customfield_10101": [{"value": "East"}, {"value": "South"}, {"value": "North"}]
    }
}"#;

fn fields() -> Fields {
    Fields { business_unit: Some("customfield_10100".into()), labels: Some("customfield_10101".into()) }
}

#[test]
fn reads_who_the_token_belongs_to() {
    let fake = Fake::new();
    fake.respond(Method::Get, "/rest/api/3/myself", 200, r#"{"accountId": "acc-me", "displayName": "Alex"}"#);
    let me = Client::new(&fake, &account()).me().unwrap();
    assert_eq!(me, Me { account_id: "acc-me".into(), name: "Alex".into() });
    assert_eq!(fake.urls(), ["https://example.atlassian.net/rest/api/3/myself"]);
}

#[test]
fn a_refused_token_is_reported_plainly() {
    let fake = Fake::new();
    fake.respond(Method::Get, "/myself", 401, "");
    assert_eq!(Client::new(&fake, &account()).me(), Err(Error::BadCredentials));
}

#[test]
fn finds_the_business_unit_fields_by_name() {
    let fake = Fake::new();
    fake.respond(
        Method::Get,
        "/rest/api/3/field",
        200,
        r#"[{"id": "summary", "name": "Summary", "custom": false},
            {"id": "labels", "name": "Labels", "custom": false},
            {"id": "customfield_10100", "name": "Business Unit", "custom": true},
            {"id": "customfield_10101", "name": "Shared Units", "custom": true}]"#,
    );
    assert_eq!(Client::new(&fake, &account()).fields("business unit", "Shared Units").unwrap(), fields());
}

#[test]
fn without_field_names_asks_jira_nothing() {
    let fake = Fake::new();
    assert_eq!(Client::new(&fake, &account()).fields("", " ").unwrap(), Fields::default());
    assert!(fake.urls().is_empty());
}

#[test]
fn maps_an_issue_with_its_epic_and_business_units() {
    let fake = Fake::new();
    fake.respond(Method::Post, "/rest/api/3/search/jql", 200, &format!(r#"{{"issues": [{ISSUE}], "isLast": true}}"#));
    let issues = Client::new(&fake, &account()).search("assignee = currentUser()", &fields(), &me(), 50).unwrap();
    assert_eq!(
        issues,
        [Issue {
            key: key("APP-123"),
            summary: "Fix the export button".into(),
            kind: IssueKind::Story,
            status: StatusCategory::InProgress,
            epic: Some(key("APP-100")),
            business_unit: Some("North".into()),
            labels: vec!["East".into(), "South".into(), "North".into()],
            assigned_to_me: true,
        }]
    );
    let body: Value = serde_json::from_str(fake.requests.lock().unwrap()[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(body["jql"], "assignee = currentUser()");
    assert!(body["fields"].as_array().unwrap().contains(&json!("customfield_10100")));
}

#[test]
fn work_log_issues_are_recognised_by_their_type_name() {
    let fake = Fake::new();
    fake.respond(
        Method::Post,
        "/search/jql",
        200,
        r#"{"issues": [{"key": "APP-5", "fields": {"summary": "Meetings", "issuetype": {"name": "Work log"},
            "status": {"statusCategory": {"key": "new"}}}}]}"#,
    );
    let issue = Client::new(&fake, &account()).search("x", &Fields::default(), &me(), 10).unwrap().remove(0);
    assert_eq!((issue.kind, issue.status, issue.assigned_to_me), (IssueKind::WorkLog, StatusCategory::ToDo, false));
}

#[test]
fn search_follows_page_tokens() {
    let fake = Fake::new();
    let page = |key: &str, next: Option<&str>| {
        format!(
            r#"{{"issues": [{{"key": "{key}", "fields": {{"summary": "s"}}}}], "nextPageToken": {}}}"#,
            next.map_or("null".to_owned(), |t| format!("\"{t}\""))
        )
    };
    fake.respond(Method::Post, "/search/jql", 200, &page("AB-1", Some("t2")));
    fake.respond(Method::Post, "/search/jql", 200, &page("AB-2", None));
    let keys: Vec<String> = Client::new(&fake, &account())
        .search("x", &Fields::default(), &me(), 50)
        .unwrap()
        .into_iter()
        .map(|i| i.key.to_string())
        .collect();
    assert_eq!(keys, ["AB-1", "AB-2"]);
    let second: Value = serde_json::from_str(fake.requests.lock().unwrap()[1].body.as_deref().unwrap()).unwrap();
    assert_eq!(second["nextPageToken"], "t2");
}

#[test]
fn reads_only_your_worklogs_inside_the_window() {
    let fake = Fake::new();
    fake.respond(Method::Post, "/search/jql", 200, r#"{"issues": [{"key": "APP-123", "fields": {}}]}"#);
    fake.respond(
        Method::Get,
        "/rest/api/3/issue/APP-123/worklog",
        200,
        r#"{"total": 3, "worklogs": [
            {"id": "101", "author": {"accountId": "acc-me"}, "started": "2026-10-26T08:00:00.000+0100",
             "timeSpentSeconds": 12600,
             "comment": {"type": "doc", "version": 1, "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Review"}]}]}},
            {"id": "102", "author": {"accountId": "someone-else"}, "started": "2026-10-26T09:00:00.000+0100", "timeSpentSeconds": 3600},
            {"id": "103", "author": {"accountId": "acc-me"}, "started": "2026-11-20T09:00:00.000+0100", "timeSpentSeconds": 3600}
        ]}"#,
    );
    let window = TimeRange::new(at("2026-10-26T00:00:00Z"), at("2026-11-02T00:00:00Z"));
    let worklogs = Client::new(&fake, &account()).worklogs(&me(), window, &oslo()).unwrap();
    assert_eq!(
        worklogs,
        [RemoteWorklog {
            id: RemoteWorklogId("101".into()),
            issue: key("APP-123"),
            range: TimeRange::starting_at(at("2026-10-26T07:00:00Z"), SignedDuration::from_mins(210)),
            comment: "Review".into(),
        }]
    );
    let jql: Value = serde_json::from_str(fake.requests.lock().unwrap()[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(jql["jql"], "worklogAuthor = currentUser() AND worklogDate >= \"2026-10-25\" AND worklogDate <= \"2026-11-03\"");
}

fn worklog(issue: &str, start: &str, minutes: i64, comment: &str) -> Worklog {
    Worklog {
        id: backfill_core::WorklogId(1),
        remote_id: None,
        issue: key(issue),
        range: TimeRange::starting_at(at(start), SignedDuration::from_mins(minutes)),
        comment: comment.into(),
        origin: None,
    }
}

fn remote(id: &str, issue: &str, start: &str, minutes: i64) -> RemoteWorklog {
    RemoteWorklog {
        id: RemoteWorklogId(id.into()),
        issue: key(issue),
        range: TimeRange::starting_at(at(start), SignedDuration::from_mins(minutes)),
        comment: String::new(),
    }
}

#[test]
fn creating_sends_local_time_with_its_offset_and_no_empty_comment() {
    let fake = Fake::new();
    fake.respond(Method::Post, "/issue/APP-123/worklog", 201, r#"{"id": "555"}"#);
    let op = SyncOp::Create(worklog("APP-123", "2026-10-26T07:00:00Z", 90, ""));
    let created = Client::new(&fake, &account()).apply(&op, &oslo()).unwrap();
    assert_eq!(created, Some(RemoteWorklogId("555".into())));
    let request = fake.requests.lock().unwrap()[0].clone();
    assert_eq!(request.url, "https://example.atlassian.net/rest/api/3/issue/APP-123/worklog?notifyUsers=false&adjustEstimate=auto");
    let body: Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
    assert_eq!(body, json!({"started": "2026-10-26T08:00:00.000+0100", "timeSpentSeconds": 5400}));
}

#[test]
fn updating_leaves_the_comment_alone_unless_it_changed() {
    let fake = Fake::new();
    fake.respond(Method::Put, "/issue/APP-123/worklog/101", 200, "{}");
    let op = SyncOp::Update {
        before: remote("101", "APP-123", "2026-10-26T07:00:00Z", 60),
        after: worklog("APP-123", "2026-10-26T07:00:00Z", 90, ""),
    };
    Client::new(&fake, &account()).apply(&op, &oslo()).unwrap();
    let request = fake.requests.lock().unwrap()[0].clone();
    let body: Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
    assert!(body.get("comment").is_none());
    assert_eq!(body["timeSpentSeconds"], 5400);
    // Changing a worklog must not hand the difference back to the remaining estimate.
    assert_eq!(request.url, "https://example.atlassian.net/rest/api/3/issue/APP-123/worklog/101?notifyUsers=false&adjustEstimate=leave");
}

#[test]
fn moving_creates_the_new_worklog_before_deleting_the_old() {
    let fake = Fake::new();
    fake.respond(Method::Post, "/issue/APP-2/worklog", 201, r#"{"id": "777"}"#);
    fake.respond(Method::Delete, "/issue/APP-1/worklog/101", 204, "");
    let op = SyncOp::Move {
        before: remote("101", "APP-1", "2026-10-26T07:00:00Z", 60),
        after: worklog("APP-2", "2026-10-26T07:00:00Z", 60, ""),
    };
    let created = Client::new(&fake, &account()).apply(&op, &oslo()).unwrap();
    assert_eq!(created, Some(RemoteWorklogId("777".into())));
    let requests = fake.requests.lock().unwrap().clone();
    let methods: Vec<Method> = requests.iter().map(|r| r.method).collect();
    assert_eq!(methods, [Method::Post, Method::Delete]);
    assert!(requests[0].url.ends_with("adjustEstimate=auto") && requests[1].url.ends_with("adjustEstimate=leave"));
}

#[test]
fn a_failed_create_in_a_move_leaves_the_old_worklog_alone() {
    let fake = Fake::new();
    fake.respond(Method::Post, "/issue/APP-2/worklog", 400, r#"{"errorMessages": [], "errors": {"timeLogged": "You do not have permission"}}"#);
    let op = SyncOp::Move {
        before: remote("101", "APP-1", "2026-10-26T07:00:00Z", 60),
        after: worklog("APP-2", "2026-10-26T07:00:00Z", 60, ""),
    };
    let error = Client::new(&fake, &account()).apply(&op, &oslo()).unwrap_err();
    assert_eq!(error, Error::Unexpected { status: 400, message: "timeLogged: You do not have permission".into() });
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
}

#[test]
fn deleting_something_already_gone_counts_as_done() {
    let fake = Fake::new();
    fake.respond(Method::Delete, "/worklog/101", 404, r#"{"errorMessages": ["Worklog not found"]}"#);
    let op = SyncOp::Delete(remote("101", "APP-1", "2026-10-26T07:00:00Z", 60));
    assert_eq!(Client::new(&fake, &account()).apply(&op, &oslo()), Ok(None));
}
