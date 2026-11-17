//! 远程桥与远程偏好页的界面测试。

use agent::message::Message;
use agent::model::ModelBuilder;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Entity, TestAppContext, point};
use std::sync::Arc;
use std::time::Duration;

use super::Haloeth;
use super::WINDOW_SIZE;
use super::tests::{
    ScriptedModel, configure, final_answer, open_shell, preferences_handle, settle_alert,
    settle_preferences, tool_call, wait_for_status,
};

/// 投递一条远程命令并等待界面线程处理完毕。
fn dispatch(
    cx: &mut TestAppContext,
    haloeth: &Entity<Haloeth>,
    command: remote::Command,
) -> Result<serde_json::Value, remote::RemoteError> {
    let mut receiver = cx.update(|cx| haloeth.read(cx).remote.dispatch_for_test(command));
    for _ in 0..100 {
        cx.run_until_parked();
        match receiver.try_recv() {
            Ok(result) => return result,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                panic!("远程命令回传通道被关闭");
            }
        }
    }
    panic!("远程命令未回传");
}

/// 注入脚本模型，供远程命令驱动的运行使用。
fn script_run(cx: &mut TestAppContext, haloeth: &Entity<Haloeth>, replies: Vec<Message>) {
    let builder: ModelBuilder = Arc::new(move || Box::new(ScriptedModel::new(replies.clone())));
    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| session.set_model_builder(builder, cx));
        });
    });
}

/// 远程消息进入对话区，同时按发布顺序广播通知。
#[gpui_kit::test]
async fn remote_message_send_reaches_the_conversation(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.executor().allow_parking();
    configure(cx, &haloeth, "local", "secret", &["m"]);
    script_run(cx, &haloeth, vec![final_answer(serde_json::json!("done"))]);

    let mut events = cx.update(|cx| haloeth.read(cx).remote.subscribe());
    dispatch(
        cx,
        &haloeth,
        remote::Command::MessageSend {
            text: "整理目录".to_owned(),
            attachments: Vec::new(),
        },
    )
    .expect("发送应当成功");
    wait_for_status(cx, handle, "就绪").await;

    cx.update(|cx| {
        let conversation = haloeth.read(cx).conversation.read(cx);
        assert_eq!(conversation.message_count(), 2, "用户消息与最终答复各一条");
    });

    let mut seen = Vec::new();
    while let Ok(published) = events.try_recv() {
        seen.push(published);
    }
    assert!(seen.windows(2).all(|pair| pair[0].seq < pair[1].seq), "通知序号严格递增");
    assert!(
        seen.iter().any(|published| matches!(
            &published.notification,
            remote::Notification::MessageAppended(appended) if appended.text == "整理目录"
        )),
        "用户消息以通知广播"
    );
    assert!(
        seen.iter().any(|published| matches!(
            &published.notification,
            remote::Notification::StatusChanged(changed)
                if changed.status == remote::proto::dto::SessionStatusDto::Preparing
        )),
        "状态先进入准备中"
    );
    assert!(
        seen.iter().any(|published| matches!(
            &published.notification,
            remote::Notification::MessageCompleted(completed)
                if completed
                    .text
                    .as_deref()
                    .is_some_and(|text| text.contains("最终答案"))
        )),
        "步骤文本由服务端渲染"
    );
    assert!(
        seen.iter().any(|published| matches!(
            &published.notification,
            remote::Notification::StatusChanged(changed)
                if changed.status == remote::proto::dto::SessionStatusDto::Idle
        )),
        "结束后回到空闲"
    );
    assert!(
        seen.iter().any(|published| matches!(
            &published.notification,
            remote::Notification::UsageChanged(_)
        )),
        "步骤完成时广播用量"
    );
}

/// 远程审批：请求经通知广播，解决后运行继续。
#[gpui_kit::test]
async fn remote_approval_resolution_reaches_the_gate(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.executor().allow_parking();
    configure(cx, &haloeth, "local", "secret", &["m"]);
    script_run(
        cx,
        &haloeth,
        vec![
            tool_call("shell", serde_json::json!({ "command": "echo hi" })),
            final_answer(serde_json::json!("done")),
        ],
    );

    let mut events = cx.update(|cx| haloeth.read(cx).remote.subscribe());
    dispatch(
        cx,
        &haloeth,
        remote::Command::MessageSend {
            text: "run".to_owned(),
            attachments: Vec::new(),
        },
    )
    .expect("发送应当成功");
    wait_for_status(cx, handle, "等待确认").await;

    let mut approval = None;
    while let Ok(published) = events.try_recv() {
        if let remote::Notification::ApprovalRequested(requested) = published.notification {
            approval = Some(requested);
        }
    }
    let approval = approval.expect("审批请求已广播");
    assert_eq!(approval.tool, "shell");
    assert!(approval.manual, "越权调用需要人工确认");
    assert_eq!(approval.risk, remote::proto::dto::RiskLevelDto::High);
    assert!(approval.effects.contains(&"command".to_owned()));

    dispatch(
        cx,
        &haloeth,
        remote::Command::ApprovalResolve {
            id: approval.id,
            decision: remote::proto::dto::ApprovalDecisionDto::AllowOnce,
        },
    )
    .expect("审批应当被接受");
    wait_for_status(cx, handle, "就绪").await;

    cx.update(|cx| {
        let label =
            haloeth.read(cx).conversation.read(cx).last_message_source().expect("最终答案可见");
        assert!(label.contains("done"), "答案内容可见：{label}");
    });

    let mut resolved = false;
    while let Ok(published) = events.try_recv() {
        if let remote::Notification::ApprovalResolved(notification) = published.notification {
            resolved = true;
            assert_eq!(notification.decision, remote::proto::dto::ApprovalDecisionDto::AllowOnce);
        }
    }
    assert!(resolved, "审批结果以通知广播");
}

/// 远程切换会话时推送新的会话快照。
#[gpui_kit::test]
async fn remote_session_switch_pushes_a_fresh_transcript(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.executor().allow_parking();
    configure(cx, &haloeth, "local", "secret", &["m"]);
    script_run(cx, &haloeth, vec![final_answer(serde_json::json!("done"))]);

    dispatch(
        cx,
        &haloeth,
        remote::Command::MessageSend {
            text: "第一条".to_owned(),
            attachments: Vec::new(),
        },
    )
    .expect("发送应当成功");
    wait_for_status(cx, handle, "就绪").await;
    let before = cx.update(|cx| haloeth.read(cx).active_id().to_owned());

    let mut events = cx.update(|cx| haloeth.read(cx).remote.subscribe());
    dispatch(cx, &haloeth, remote::Command::SessionNew).expect("新建会话应当成功");
    let after = cx.update(|cx| haloeth.read(cx).active_id().to_owned());
    assert_ne!(before, after, "当前会话应当切换");

    let mut reset = None;
    while let Ok(published) = events.try_recv() {
        if let remote::Notification::TranscriptReset(notification) = published.notification {
            reset = Some(notification);
        }
    }
    let reset = reset.expect("切换会话推送快照");
    assert_eq!(reset.session_id, after);
    assert!(reset.entries.is_empty(), "新会话的显示记录为空");

    let state = dispatch(cx, &haloeth, remote::Command::StateGet).expect("状态读取应当成功");
    assert_eq!(state["state"]["active_session_id"], serde_json::json!(after));
}

/// 功能开关、目录授权与模型选择通过远程命令生效。
#[gpui_kit::test]
fn remote_feature_grants_and_model_selection_apply(cx: &mut TestAppContext) {
    let (haloeth, _handle) = open_shell(cx, WINDOW_SIZE);
    cx.executor().allow_parking();
    configure(cx, &haloeth, "local", "secret", &["a", "b"]);

    dispatch(
        cx,
        &haloeth,
        remote::Command::FeatureSet {
            key: "tool:web_search".to_owned(),
            enabled: false,
        },
    )
    .expect("功能开关应当成功");
    cx.update(|cx| {
        let session = haloeth.read(cx).session.read(cx);
        assert!(!session.feature_state(crate::config::FeatureKind::Tool, "web_search"));
    });

    let directory =
        std::env::temp_dir().join(format!("haloeth-remote-grant-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("临时目录");
    let raw = directory.display().to_string();
    dispatch(
        cx,
        &haloeth,
        remote::Command::GrantAdd {
            path: raw.clone(),
            access: remote::proto::dto::AccessDto::Read,
        },
    )
    .expect("目录授权应当成功");
    cx.update(|cx| {
        let grants = haloeth.read(cx).session.read(cx).grants().to_vec();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].path.file_name(), directory.file_name());
    });

    dispatch(
        cx,
        &haloeth,
        remote::Command::GrantRemove {
            path: raw.clone(),
        },
    )
    .expect("移除授权应当成功");
    cx.update(|cx| {
        assert!(haloeth.read(cx).session.read(cx).grants().is_empty());
    });

    dispatch(
        cx,
        &haloeth,
        remote::Command::ModelSelect {
            provider: "local".to_owned(),
            model: "b".to_owned(),
        },
    )
    .expect("模型选择应当成功");
    cx.update(|cx| {
        assert_eq!(haloeth.read(cx).session.read(cx).selection(), Some(("local", "b")));
    });

    let error = dispatch(
        cx,
        &haloeth,
        remote::Command::ModelSelect {
            provider: "local".to_owned(),
            model: "missing".to_owned(),
        },
    )
    .expect_err("未知模型应当报错");
    assert_eq!(error.code, remote::ErrorCode::UnknownModel);

    let error = dispatch(
        cx,
        &haloeth,
        remote::Command::FeatureSet {
            key: "video:foo".to_owned(),
            enabled: true,
        },
    )
    .expect_err("未知功能类别应当报错");
    assert_eq!(error.code, remote::ErrorCode::UnknownFeature);

    let error = dispatch(
        cx,
        &haloeth,
        remote::Command::GrantAdd {
            path: directory.join("missing").display().to_string(),
            access: remote::proto::dto::AccessDto::Read,
        },
    )
    .expect_err("不存在的目录应当报错");
    assert_eq!(error.code, remote::ErrorCode::InvalidParams);

    let _ = std::fs::remove_dir_all(&directory);
}

/// 握手体与快照使用实时状态。
#[gpui_kit::test]
fn remote_handshake_payloads_use_live_state(cx: &mut TestAppContext) {
    let (haloeth, _handle) = open_shell(cx, WINDOW_SIZE);
    cx.executor().allow_parking();

    let welcome = dispatch(cx, &haloeth, remote::Command::Welcome).expect("握手体应当成功");
    assert_eq!(welcome["protocol"], serde_json::json!(1));
    assert_eq!(welcome["server"]["name"], serde_json::json!("haloeth"));
    assert!(welcome["state"]["active_session_id"].is_string());
    assert!(welcome["sessions"].is_array());

    let snapshot = dispatch(cx, &haloeth, remote::Command::Snapshot).expect("快照应当成功");
    assert!(snapshot["watermark"].is_u64(), "水位是计数器整数");
    assert!(snapshot["entries"].is_array());
}

/// 远程偏好页生成配对信息并管理设备。
#[gpui_kit::test]
async fn remote_preferences_page_manages_devices(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::Remote, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("remote-panel").visible(), "远程分页可见");
        window.click("remote-tls", cx);
        window.render_frame(cx);
        window.click("remote-device-name", cx);
        window.within("remote-panel").input("iPhone", cx);
        window.click("remote-device-add", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let remote = haloeth.read(cx).session.read(cx).config().remote.clone();
        assert!(remote.tls, "传输加密开关即时生效");
        assert_eq!(remote.devices.len(), 1);
        assert_eq!(remote.devices[0].name, "iPhone");
        assert_eq!(remote.devices[0].token_hash.len(), 64, "只保存摘要");
    });

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("remote-qr").visible(), "配对后显示二维码");
        let pairing = window.find("remote-pairing").label().unwrap_or_default().to_owned();
        assert!(pairing.starts_with("haloeth://pair?"), "配对串使用自定义 scheme：{pairing}");
    })
    .unwrap();

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        window.scroll("pref-scroll", gpui_kit::ScrollDelta::Lines(point(0., -40.)), cx);
        window.render_frame(cx);
        window.click("remote-revoke:0", cx);
    })
    .unwrap();
    settle_alert(cx, prefs).await;
    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(
            haloeth.read(cx).session.read(cx).config().remote.devices.is_empty(),
            "确认后设备被移除"
        );
    });
}

/// 运行中拒绝新消息与会话切换，附件参数尚未实现。
#[gpui_kit::test]
async fn remote_rejects_busy_submissions_and_attachments(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.executor().allow_parking();
    configure(cx, &haloeth, "local", "secret", &["m"]);
    script_run(
        cx,
        &haloeth,
        vec![
            tool_call("shell", serde_json::json!({ "command": "echo hi" })),
            final_answer(serde_json::json!("done")),
        ],
    );

    dispatch(
        cx,
        &haloeth,
        remote::Command::MessageSend {
            text: "first".to_owned(),
            attachments: Vec::new(),
        },
    )
    .expect("首次发送应当成功");
    wait_for_status(cx, handle, "等待确认").await;

    let error = dispatch(
        cx,
        &haloeth,
        remote::Command::MessageSend {
            text: "second".to_owned(),
            attachments: Vec::new(),
        },
    )
    .expect_err("运行中拒绝新消息");
    assert_eq!(error.code, remote::ErrorCode::Busy);
    assert_eq!(
        error.data.as_ref().and_then(|data| data.get("text")),
        Some(&serde_json::json!("second")),
        "回显被拒绝的文本"
    );

    let error =
        dispatch(cx, &haloeth, remote::Command::SessionNew).expect_err("运行中拒绝切换会话");
    assert_eq!(error.code, remote::ErrorCode::Busy);

    dispatch(cx, &haloeth, remote::Command::RunCancel).expect("取消应当成功");
    wait_for_status(cx, handle, "就绪").await;

    let error = dispatch(
        cx,
        &haloeth,
        remote::Command::MessageSend {
            text: "带附件".to_owned(),
            attachments: vec!["attachment-1".to_owned()],
        },
    )
    .expect_err("附件路径不存在");
    assert_eq!(error.code, remote::ErrorCode::InvalidParams);
}

/// 带附件的消息把路径引用到用户消息，并给附件目录补充只读授权。
#[gpui_kit::test]
async fn remote_message_send_with_attachments_adds_references(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.executor().allow_parking();
    configure(cx, &haloeth, "local", "secret", &["m"]);
    script_run(cx, &haloeth, vec![final_answer(serde_json::json!("done"))]);

    let dir = std::env::temp_dir().join(format!("haloeth-attachment-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("临时目录");
    let file = dir.join("note.txt");
    std::fs::write(&file, "你好").expect("写入附件");

    dispatch(
        cx,
        &haloeth,
        remote::Command::MessageSend {
            text: "看看这个".to_owned(),
            attachments: vec![file.display().to_string()],
        },
    )
    .expect("带附件的发送应当成功");
    wait_for_status(cx, handle, "就绪").await;

    cx.update(|cx| {
        let entries = haloeth.read(cx).conversation.read(cx).snapshot(cx);
        let user = entries
            .iter()
            .find_map(|entry| match entry {
                crate::ui::conversation::TranscriptEntry::Text {
                    role: crate::ui::conversation::ChatRole::User,
                    text,
                } => Some(text.clone()),
                _ => None,
            })
            .expect("用户消息可见");
        assert!(user.starts_with("[附件] "), "引用行在消息开头：{user}");
        assert!(user.contains("note.txt"), "引用路径完整：{user}");
        assert!(user.contains("看看这个"), "原文保留：{user}");
        let grants = haloeth.read(cx).session.read(cx).grants().to_vec();
        assert!(
            grants.iter().any(|grant| {
                grant.access == agent::tool::Access::Read
                    && grant.path.file_name() == dir.file_name()
            }),
            "附件目录获得只读授权：{grants:?}"
        );
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// 桌面拖入文件：复制到会话附件目录并在输入框中引用。
#[gpui_kit::test]
fn dropping_files_attaches_them_to_the_composer(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    let dir = std::env::temp_dir().join(format!("haloeth-drop-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("临时目录");
    let file = dir.join("shot.txt");
    std::fs::write(&file, "截图说明").expect("写入附件");

    let path = file.clone();
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.attach_paths(std::slice::from_ref(&path), window, cx);
        });
    })
    .unwrap();

    let session_id = cx.update(|cx| haloeth.read(cx).active_id().to_owned());
    cx.update(|cx| {
        let value = haloeth.read(cx).composer.read(cx).buffer().read(cx).value().to_string();
        assert!(value.contains("[附件] "), "输入框包含引用：{value}");
        assert!(value.contains("shot.txt"), "引用保留文件名：{value}");
        let grants = haloeth.read(cx).session.read(cx).grants().to_vec();
        assert!(
            grants.iter().any(|grant| grant.access == agent::tool::Access::Read),
            "附件目录获得只读授权"
        );
    });

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(crate::remote::attachments_dir(false).join(session_id));
}

/// 增量续接：游标可覆盖时按原序号补发，否则回退快照。
#[gpui_kit::test]
fn remote_resume_replays_recent_notifications(cx: &mut TestAppContext) {
    let (haloeth, _handle) = open_shell(cx, WINDOW_SIZE);
    cx.executor().allow_parking();
    let (session_id, watermark) = cx.update(|cx| {
        (haloeth.read(cx).active_id().to_owned(), haloeth.read(cx).remote.watermark())
    });
    assert!(watermark >= 1, "启动阶段已产生通知");

    let replay = dispatch(
        cx,
        &haloeth,
        remote::Command::Resume {
            session_id: session_id.clone(),
            last_seq: watermark - 1,
        },
    )
    .expect("补发应当成功");
    let frames = replay["notifications"].as_array().expect("返回补发帧");
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["seq"], serde_json::json!(watermark));

    let fallback = dispatch(
        cx,
        &haloeth,
        remote::Command::Resume {
            session_id: "missing".to_owned(),
            last_seq: 0,
        },
    )
    .expect("回退应当成功");
    assert!(fallback["snapshot"].is_object(), "会话不匹配回退快照");
}
