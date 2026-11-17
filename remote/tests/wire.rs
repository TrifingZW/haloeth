//! 线格式黄金快照：字段改名或枚举值变化会被这里捕获。

use serde_json::{Value, json};

use remote::proto::dto::*;
use remote::proto::{
    Command, ErrorCode, Notification, NotificationFrame, RemoteError, RequestFrame, ResponseFrame,
    TranscriptReset,
};

#[test]
fn notification_frames_match_the_wire_envelope() {
    let notification = Notification::MessageDelta(remote::proto::notify::MessageDelta {
        text: "正在".to_owned(),
    });
    let frame = NotificationFrame::new(42, &notification);
    let text = serde_json::to_string(&frame).expect("序列化应当成功");
    assert_eq!(text, r#"{"v":1,"seq":42,"op":"message.delta","params":{"text":"正在"}}"#);
}

#[test]
fn every_notification_keeps_its_documented_op_name() {
    let cases: Vec<(Notification, &str)> = vec![
        (
            Notification::TranscriptReset(TranscriptReset {
                session_id: "s1".into(),
                entries: Vec::new(),
                streaming: None,
            }),
            "transcript.reset",
        ),
        (
            Notification::MessageAppended(remote::proto::notify::MessageAppended {
                role: ChatRoleDto::User,
                text: "hi".into(),
            }),
            "message.appended",
        ),
        (
            Notification::MessageDelta(remote::proto::notify::MessageDelta {
                text: "x".into(),
            }),
            "message.delta",
        ),
        (
            Notification::MessageCompleted(remote::proto::notify::MessageCompleted {
                text: None,
            }),
            "message.completed",
        ),
        (
            Notification::ReasoningDelta(remote::proto::notify::ReasoningDelta {
                text: "x".into(),
            }),
            "reasoning.delta",
        ),
        (
            Notification::ToolStarted(remote::proto::notify::ToolStarted {
                call_id: "c1".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }),
            "tool.started",
        ),
        (
            Notification::ToolFinished(remote::proto::notify::ToolFinished {
                call_id: "c1".into(),
                ok: true,
                preview: "done".into(),
                elapsed_ms: 5,
            }),
            "tool.finished",
        ),
        (
            Notification::ApprovalRequested(remote::proto::notify::ApprovalRequested {
                id: "a1".into(),
                tool: "write_file".into(),
                summary: "写文件".into(),
                risk: RiskLevelDto::High,
                manual: true,
                effects: vec!["write".into()],
            }),
            "approval.requested",
        ),
        (
            Notification::ApprovalResolved(remote::proto::notify::ApprovalResolved {
                id: "a1".into(),
                decision: ApprovalDecisionDto::AllowOnce,
                reason: String::new(),
            }),
            "approval.resolved",
        ),
        (
            Notification::ImagesAppended(remote::proto::notify::ImagesAppended {
                items: vec![ImageItemDto {
                    path: "C:/out.png".into(),
                    caption: "图".into(),
                }],
            }),
            "images.appended",
        ),
        (
            Notification::PlanAppended(remote::proto::notify::PlanAppended {
                text: "计划".into(),
            }),
            "plan.appended",
        ),
        (
            Notification::StatusChanged(remote::proto::notify::StatusChanged {
                status: SessionStatusDto::Running,
                activity: Some("思考中…".into()),
            }),
            "status.changed",
        ),
        (
            Notification::UsageChanged(remote::proto::notify::UsageChanged {
                total: UsagePairDto {
                    input: 1,
                    output: 2,
                },
                by_model: Default::default(),
            }),
            "usage.changed",
        ),
        (
            Notification::SessionsChanged(remote::proto::notify::SessionsChanged {
                summaries: Vec::new(),
            }),
            "sessions.changed",
        ),
        (
            Notification::StateChanged(remote::proto::notify::StateChanged {
                state: sample_state(),
            }),
            "state.changed",
        ),
        (
            Notification::Shutdown(remote::proto::notify::Shutdown {
                reason: "disabled".into(),
            }),
            "shutdown",
        ),
    ];
    for (notification, expected) in cases {
        let (op, _) = notification.envelope();
        assert_eq!(op, expected, "通知 {expected} 的操作名保持稳定");
    }
}

#[test]
fn notifications_round_trip_through_envelopes() {
    let notification = Notification::ToolFinished(remote::proto::notify::ToolFinished {
        call_id: "c1".into(),
        ok: false,
        preview: "错误".into(),
        elapsed_ms: 9,
    });
    let (op, params) = notification.envelope();
    let restored = Notification::from_envelope(&op, params).expect("应当能还原");
    assert_eq!(restored, notification);
}

#[test]
fn requests_parse_into_typed_commands() {
    let parse = |value: Value| {
        Command::parse(&serde_json::from_value::<RequestFrame>(value).expect("信封"))
    };

    assert_eq!(parse(json!({"v":1,"id":1,"op":"ping"})), Ok(Command::Ping));
    assert_eq!(
        parse(json!({"v":1,"id":1,"op":"message.send","params":{"text":"  你好  "}})),
        Ok(Command::MessageSend {
            text: "你好".to_owned(),
            attachments: Vec::new(),
        })
    );
    assert_eq!(
        parse(
            json!({"v":1,"id":1,"op":"approval.resolve","params":{"id":"a1","decision":"allow_always"}})
        ),
        Ok(Command::ApprovalResolve {
            id: "a1".to_owned(),
            decision: ApprovalDecisionDto::AllowAlways,
        })
    );
    assert_eq!(
        parse(json!({"v":1,"id":1,"op":"grant.add","params":{"path":"D:/work","access":"read"}})),
        Ok(Command::GrantAdd {
            path: "D:/work".to_owned(),
            access: AccessDto::Read,
        })
    );
}

#[test]
fn invalid_requests_report_structured_errors() {
    let parse = |value: Value| {
        Command::parse(&serde_json::from_value::<RequestFrame>(value).expect("信封"))
    };

    let unknown =
        parse(json!({"v":1,"id":1,"op":"session.resume","params":{}})).expect_err("预留操作");
    assert_eq!(unknown.code, ErrorCode::UnknownOp);

    let empty = parse(json!({"v":1,"id":1,"op":"message.send","params":{"text":"   "}}))
        .expect_err("空文本");
    assert_eq!(empty.code, ErrorCode::InvalidParams);

    let missing = parse(json!({"v":1,"id":1,"op":"session.open","params":{}})).expect_err("缺参数");
    assert_eq!(missing.code, ErrorCode::InvalidParams);

    let blank_title = parse(json!({"v":1,"id":1,"op":"session.rename","params":{"title":" "}}))
        .expect_err("空标题");
    assert_eq!(blank_title.code, ErrorCode::InvalidParams);
}

#[test]
fn busy_errors_echo_the_rejected_text() {
    let error = RemoteError::busy("整理目录");
    let value = serde_json::to_value(&error).expect("序列化");
    assert_eq!(
        value,
        json!({
            "code": "busy",
            "message": "运行中",
            "data": { "text": "整理目录" },
        })
    );
}

#[test]
fn responses_keep_success_and_failure_shapes_distinct() {
    let ok = serde_json::to_value(ResponseFrame::ok(7, json!({}))).expect("序列化");
    assert_eq!(ok, json!({"v":1,"id":7,"ok":true,"result":{}}));

    let failed = serde_json::to_value(ResponseFrame::err(8, RemoteError::invalid_params("坏参数")))
        .expect("序列化");
    assert_eq!(
        failed,
        json!({
            "v": 1,
            "id": 8,
            "ok": false,
            "error": { "code": "invalid_params", "message": "坏参数" },
        })
    );
}

#[test]
fn transcript_entries_use_tagged_wire_shapes() {
    let text = serde_json::to_value(TranscriptEntryDto::Text {
        role: ChatRoleDto::User,
        text: "列出目录".into(),
    })
    .expect("序列化");
    assert_eq!(text, json!({"type":"text","role":"user","text":"列出目录"}));

    let activity = serde_json::to_value(TranscriptEntryDto::Activity(ActivityRecordDto {
        id: "call_1".into(),
        title: "调用 list_dir".into(),
        detail: "{}".into(),
        preview: None,
        elapsed_ms: Some(3),
        status: ActivityStatusDto::Ok,
        expanded: false,
    }))
    .expect("序列化");
    assert_eq!(
        activity,
        json!({
            "type": "activity",
            "id": "call_1",
            "title": "调用 list_dir",
            "detail": "{}",
            "elapsed_ms": 3,
            "status": "ok",
            "expanded": false,
        })
    );

    let image = serde_json::to_value(TranscriptEntryDto::Image {
        path: "C:/out.png".into(),
        caption: "图".into(),
    })
    .expect("序列化");
    assert_eq!(image, json!({"type":"image","path":"C:/out.png","caption":"图"}));

    let restored: TranscriptEntryDto =
        serde_json::from_value(activity).expect("应当能读回活动记录");
    assert_eq!(restored_type(restored), "activity");
}

fn restored_type(entry: TranscriptEntryDto) -> &'static str {
    match entry {
        TranscriptEntryDto::Text {
            ..
        } => "text",
        TranscriptEntryDto::Activity(_) => "activity",
        TranscriptEntryDto::Image {
            ..
        } => "image",
    }
}

#[test]
fn app_state_serialises_with_the_documented_fields() {
    let value = serde_json::to_value(sample_state()).expect("序列化");
    assert_eq!(value["active_session_id"], json!("s1"));
    assert_eq!(value["status"], json!("idle"));
    assert_eq!(value["selection"], json!({"provider":"p","model":"m"}));
    assert_eq!(value["models"], json!([{"provider":"p","model":"m"}]));
    assert_eq!(value["features"][0]["kind"], json!("tool"));
    assert_eq!(value["grants"][0]["access"], json!("write"));
    assert_eq!(value["approval_mode"], json!("rules"));
    assert_eq!(value["usage"]["total"], json!({"input":10,"output":20}));
}

#[test]
fn tokens_are_long_random_and_verify_against_hashes() {
    let token = remote::auth::new_token();
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    let other = remote::auth::new_token();
    assert_ne!(token, other);

    let hash = remote::auth::hash_token(&token);
    assert_eq!(hash.len(), 64);
    assert!(remote::auth::verify(&token, &hash));
    assert!(!remote::auth::verify(&other, &hash));
    assert!(remote::auth::verify("raw-secret", &remote::auth::hash_token("raw-secret")));
}

fn sample_state() -> AppStateDto {
    AppStateDto {
        active_session_id: "s1".into(),
        title: "整理目录".into(),
        status: SessionStatusDto::Idle,
        activity: None,
        selection: Some(ModelRefDto {
            provider: "p".into(),
            model: "m".into(),
        }),
        models: vec![ModelRefDto {
            provider: "p".into(),
            model: "m".into(),
        }],
        features: vec![FeatureDto {
            key: "tool:web_search".into(),
            kind: FeatureKindDto::Tool,
            name: "web_search".into(),
            enabled: true,
        }],
        grants: vec![GrantDto {
            path: "D:/work".into(),
            access: AccessDto::Write,
        }],
        approval_mode: ApprovalModeDto::Rules,
        usage: UsageDto {
            total: UsagePairDto {
                input: 10,
                output: 20,
            },
            by_model: Default::default(),
        },
    }
}
