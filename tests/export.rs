//! End-to-end checks against the Telegram example shipped in the repository.

use rooftop::chat::{Chat, Event};
use rooftop::import::import_dir;
use rooftop::model::{MediaKind, Message};
use rooftop::prompt::{PromptOptions, build, search};
use serde_json::Value;
use std::path::Path;

const DIR: &str = "examples/telegram";

fn export() -> Option<rooftop::model::ChatExport> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(DIR);
    if !p.join("messages.html").exists() {
        eprintln!("examples/telegram missing, skipping");
        return None;
    }
    Some(import_dir(&p, None).expect("import"))
}

fn chat(persona: &str, user: &str, max_id: i64) -> Chat {
    Chat {
        id: "t".into(),
        export_id: "e".into(),
        title: persona.into(),
        persona: persona.into(),
        user: user.into(),
        notes: String::new(),
        voice: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
        events: Vec::new(),
        next_id: max_id + 1,
        memory: None,
        last_usage: None,
        guest: None,
        generated: Default::default(),
    }
}

fn opts(budget: usize) -> PromptOptions {
    PromptOptions { budget_chars: budget, ..Default::default() }
}

fn msg(e: &rooftop::model::ChatExport, id: i64) -> &Message {
    e.messages.iter().find(|m| m.id == id).expect("message")
}

#[test]
fn parses_every_interaction_kind() {
    let Some(e) = export() else { return };
    assert_eq!(e.title, "Jonas Weber");
    let names: Vec<&str> = e.participants.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Jonas Weber", "Priya Nair"]);
    assert_eq!(e.exporter(), Some("Priya Nair"));
    assert_eq!(e.messages.len(), 76);
    // Joined messages inherit the previous sender.
    assert_eq!(msg(&e, 52106).from, "Jonas Weber");
    assert_eq!(msg(&e, 52114).from, "Priya Nair");
    // Replies, edits, multi-line text.
    assert_eq!(msg(&e, 52123).reply_to, Some(52122));
    assert!(msg(&e, 52168).edited);
    assert_eq!(
        msg(&e, 52119).text,
        "https://nonnas-kitchen.blog/green-spinach-lasagna\n\nBackup plan, in case she refuses:\n1. Read this.\n2. Do not improvise."
    );
    assert_eq!(msg(&e, 52101).date.as_deref(), Some("2026-10-16T19:02:11"));
    // Reactions.
    assert_eq!(msg(&e, 52122).reactions[0].emoji, "❤");
    assert_eq!(msg(&e, 52111).reactions[0].emoji, "🔥");
    // Media kinds.
    let kind = |id| msg(&e, id).media.as_ref().map(|m| m.kind());
    assert_eq!(kind(52106), Some(MediaKind::Sticker));
    assert_eq!(msg(&e, 52106).media.as_ref().unwrap().file.as_deref(), Some("stickers/52106_sticker.webm"));
    assert_eq!(kind(52131), Some(MediaKind::Sticker));
    assert_eq!(msg(&e, 52131).media.as_ref().unwrap().file.as_deref(), Some("stickers/52131_AnimatedSticker.tgs"));
    assert_eq!(kind(52153), Some(MediaKind::Sticker));
    assert_eq!(msg(&e, 52153).media.as_ref().unwrap().file.as_deref(), Some("stickers/52153_sticker.webp"));
    assert_eq!(kind(52122), Some(MediaKind::Photo));
    assert_eq!(kind(52172), Some(MediaKind::Video));
    assert_eq!(kind(52128), Some(MediaKind::File));
    assert_eq!(kind(52165), Some(MediaKind::Audio));
    assert_eq!(kind(52115), Some(MediaKind::Gif));
    assert_eq!(kind(52144), Some(MediaKind::Voice));
    assert_eq!(kind(52135), Some(MediaKind::RoundVideo));
    // Forwarded post with rich text.
    let fwd = msg(&e, 52158);
    assert_eq!(fwd.forwarded_from.as_deref(), Some("Kitchen Science"));
    assert!(fwd.text.starts_with("Why lasagna needs to rest"));
    assert!(fwd.text.contains("More in our guide: kitchenscience.example/rest."), "rich text (bold, links) flattens to plain text");
    // Link preview.
    let lp = msg(&e, 52119).link_preview.as_ref().expect("preview");
    assert_eq!(lp.site.as_deref(), Some("Nonna's Kitchen"));
    // Sticker thumbnails resolved from thumbs/ even though HTML doesn't reference them.
    for id in [52106, 52131, 52153] {
        let st = e.library.iter().find(|i| i.uses.contains(&id)).unwrap();
        assert_eq!(st.thumb.as_deref(), Some(format!("thumbs/thumb_{id}_0.jpg").as_str()));
    }
    let voice = e.library.iter().find(|i| i.kind == MediaKind::Voice).unwrap();
    assert!(voice.available);
    assert_eq!(lp.photo.as_deref(), Some("webpage_thumbs/webpage_52119_0.jpg"));
}

fn assert_valid_openai_sequence(messages: &[Value]) {
    assert_eq!(messages[0]["role"], "system");
    let mut i = 1;
    while i < messages.len() {
        let m = &messages[i];
        match m["role"].as_str().unwrap() {
            "user" => assert!(m["content"].as_str().is_some_and(|c| !c.is_empty())),
            "assistant" => {
                let calls = m["tool_calls"].as_array().expect("tool calls");
                assert!(!calls.is_empty());
                for (k, c) in calls.iter().enumerate() {
                    let t = &messages[i + 1 + k];
                    assert_eq!(t["role"], "tool");
                    assert_eq!(t["tool_call_id"], c["id"]);
                    let args: Value = serde_json::from_str(c["function"]["arguments"].as_str().unwrap()).unwrap();
                    assert!(args.is_object());
                }
                i += calls.len();
            }
            other => panic!("unexpected role {other} at {i}"),
        }
        i += 1;
    }
}

#[test]
fn builds_valid_messages_both_ways() {
    let Some(e) = export() else { return };
    let max = e.messages.iter().map(|m| m.id).max().unwrap();
    for (persona, user) in [("Priya Nair", "Jonas Weber"), ("Jonas Weber", "Priya Nair")] {
        let c = chat(persona, user, max);
        let p = build(&e, &c, &opts(1_000_000));
        assert_eq!(p.dropped_turns, 0);
        assert_valid_openai_sequence(&p.messages);
        let calls: Vec<(String, Value)> = p
            .messages
            .iter()
            .filter_map(|m| m["tool_calls"].as_array())
            .flatten()
            .map(|c| {
                (
                    c["function"]["name"].as_str().unwrap().to_string(),
                    serde_json::from_str(c["function"]["arguments"].as_str().unwrap()).unwrap(),
                )
            })
            .collect();
        let user_text: String =
            p.messages.iter().filter(|m| m["role"] == "user").map(|m| m["content"].as_str().unwrap().to_string()).collect();
        if persona == "Priya Nair" {
            assert!(calls.iter().any(|(n, a)| n == "forward_message" && a["message_id"] == 52158));
            assert!(calls.iter().any(|(n, a)| n == "react" && a["message_id"] == 52122 && a["emoji"] == "❤"));
            assert!(calls.iter().any(|(n, a)| n == "send_message" && a["reply_to"] == 52122));
            assert!(user_text.contains("reacted 🔥 to #52111"));
            assert!(user_text.contains("<GIF gif_"));
        } else {
            assert!(calls.iter().any(|(n, _)| n == "send_sticker"));
            assert!(calls.iter().any(|(n, _)| n == "send_gif"));
            assert!(calls.iter().any(|(n, a)| n == "react" && a["message_id"] == 52111 && a["emoji"] == "🔥"));
            assert!(user_text.contains("reacted 😂 to #52107"));
            let sys = p.messages[0]["content"].as_str().unwrap();
            assert!(sys.starts_with("You are Jonas Weber"));
        }
    }
}

#[test]
fn truncates_history_by_budget_and_keeps_sequence_valid() {
    let Some(e) = export() else { return };
    let max = e.messages.iter().map(|m| m.id).max().unwrap();
    let c = chat("Jonas Weber", "Priya Nair", max);
    let p = build(&e, &c, &opts(4_000));
    assert!(p.dropped_turns > 0);
    assert_valid_openai_sequence(&p.messages);
    assert!(p.messages[0]["content"].as_str().unwrap().contains("search_history"));
}

#[test]
fn live_events_continue_the_timeline() {
    let Some(e) = export() else { return };
    let max = e.messages.iter().map(|m| m.id).max().unwrap();
    let mut c = chat("Jonas Weber", "Priya Nair", max);
    c.events.push(Event::Message(Message {
        id: max + 1,
        from: "Priya Nair".into(),
        date: Some("2026-10-20T18:01:00".into()),
        text: "miss you".into(),
        reply_to: Some(52110),
        ..Default::default()
    }));
    c.events
        .push(Event::Reaction { target: max + 1, emoji: "❤".into(), from: "Jonas Weber".into(), date: String::new(), removed: false });
    let p = build(&e, &c, &opts(1_000_000));
    assert_valid_openai_sequence(&p.messages);
    let last_user = p.messages.iter().rev().find(|m| m["role"] == "user").unwrap();
    assert!(
        last_user["content"]
            .as_str()
            .unwrap()
            .contains(&format!("#{} 18:01 ↩ reply to #52110 (your: \"thats why i need you or nah\"): miss you", max + 1))
    );
    let last = p.messages.iter().rev().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(last["tool_calls"][0]["function"]["name"], "react");
    assert_eq!(search(&e, &c, "tiramisu", 5).len(), 1);
}

fn live_msg(id: i64, from: &str, date: &str, text: &str) -> Event {
    Event::Message(Message { id, from: from.into(), date: Some(date.into()), text: text.into(), ..Default::default() })
}

#[test]
fn system_prompt_is_static_and_prefix_is_cache_stable() {
    let Some(e) = export() else { return };
    let max = e.messages.iter().map(|m| m.id).max().unwrap();
    let mut c = chat("Jonas Weber", "Priya Nair", max);
    c.events.push(live_msg(max + 1, "Priya Nair", "2026-10-21T09:00:00", "morning"));
    let a = build(&e, &c, &opts(1_000_000));
    let sys = a.messages[0]["content"].as_str().unwrap();
    assert!(!sys.contains("Current local date"), "no clock in the system prompt");
    assert!(!sys.contains("2026"), "no dates in the system prompt");
    c.events.push(live_msg(max + 2, "Jonas Weber", "2026-10-21T09:01:00", "hey"));
    c.events.push(live_msg(max + 3, "Priya Nair", "2026-10-21T09:02:00", "how are you?"));
    let b = build(&e, &c, &opts(1_000_000));
    // The previous request is a byte-identical prefix of the next one.
    assert_eq!(&b.messages[..a.messages.len()], &a.messages[..]);
    let last = b.messages.last().unwrap()["content"].as_str().unwrap();
    assert!(last.contains("09:02: how are you?"));
    // The day separator carries the date instead of the system prompt.
    assert!(a.messages.iter().any(|m| m["content"].as_str().is_some_and(|t| t.contains("— Wednesday, 21 October 2026 —"))));
}

#[test]
fn compaction_is_planned_then_memory_keeps_prefix_stable() {
    let Some(e) = export() else { return };
    let max = e.messages.iter().map(|m| m.id).max().unwrap();
    let mut c = chat("Jonas Weber", "Priya Nair", max);
    let budget = 4_000;
    let p = build(&e, &c, &opts(budget));
    let plan = p.compaction.clone().expect("history over budget needs compaction");
    assert_eq!(plan.from, 0);
    assert!(plan.to > 0 && plan.to < p.total_turns);
    assert!(plan.transcript.contains("Jonas Weber"));
    assert!(plan.transcript.contains("Define emergency."));
    let req = rooftop::prompt::summary_request(&c, None, &plan.transcript);
    assert_eq!(req.len(), 2);

    c.memory = Some(rooftop::chat::Memory { turns: plan.to, summary: "Jonas is cooking lasagna.".into(), updated_at: String::new() });
    let a = build(&e, &c, &opts(budget));
    assert!(a.compaction.is_none(), "fits after compaction");
    assert_eq!(a.dropped_turns, plan.to);
    assert_valid_openai_sequence(&a.messages);
    let first = a.messages[1]["content"].as_str().unwrap();
    assert_eq!(a.messages[1]["role"], "user");
    assert!(first.contains("Jonas is cooking lasagna.") && first.contains("search_history"));
    assert!(first.contains("— "), "kept history starts with the current day");

    // A few more turns: still under the budget, the prefix must not move.
    c.events.push(live_msg(max + 1, "Priya Nair", "2026-10-21T09:00:00", "ok"));
    c.events.push(live_msg(max + 2, "Jonas Weber", "2026-10-21T09:01:00", "ok"));
    c.events.push(live_msg(max + 3, "Priya Nair", "2026-10-21T09:02:00", "bye"));
    let b = build(&e, &c, &opts(budget));
    assert!(b.compaction.is_none());
    assert_eq!(&b.messages[..a.messages.len()], &a.messages[..]);

    // Stale memory (e.g. after events were removed) is ignored, not trusted.
    c.memory.as_mut().unwrap().turns = 10_000;
    assert_eq!(build(&e, &c, &opts(1_000_000)).dropped_turns, 0);
}
