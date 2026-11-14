//! The English interface end to end: what the model is sent and what the core says.
//!
//! This file holds ONE test on purpose. It switches the language of the
//! process, and the tests of one file share a process and run side by side. A
//! file under `tests/` is a program of its own, so the unit tests of the crate,
//! which assert Russian texts, never see the switch. Do not add a second test here.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use mika_core::agent::{system_prompt, Agent};
use mika_core::config::AssistantConfig;
use mika_core::gate::{Gate, GateDecision};
use mika_core::i18n::prompt::REPLY_NOTE_EN;
use mika_core::i18n::{self, Lang};
use mika_core::llm::{LlmChunk, LlmError, LlmProvider, LlmRequest, LlmStream, Role};
use mika_core::session::answer_hint;
use mika_core::task::TASK_RULES;
use mika_core::tools::{Assessment, ToolRegistry};
use mika_core::{EventBus, StateHandle};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

/// No tool is called here; a gate is still required to build an agent.
struct DenyAll;

#[async_trait]
impl Gate for DenyAll {
    async fn check(
        &self,
        _tool: &str,
        _args: &serde_json::Value,
        _assessment: &Assessment,
    ) -> GateDecision {
        GateDecision::Denied("no tools in this test".into())
    }
}

/// Keeps the system text of every request and answers with one word.
struct Recorder(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl LlmProvider for Recorder {
    fn name(&self) -> &str {
        "fake"
    }

    async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError> {
        let system = req
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .map(|m| m.content.clone())
            .unwrap_or_default();
        self.0.lock().push(system);
        Ok(futures::stream::iter(vec![Ok(LlmChunk::Text("ok".into()))]).boxed())
    }
}

fn cyrillic(text: &str) -> bool {
    text.chars().any(|c| ('\u{0400}'..='\u{04ff}').contains(&c))
}

#[tokio::test]
async fn the_english_interface_changes_the_prompt_and_the_texts_of_the_core() {
    let bus = EventBus::new(64);
    let state = StateHandle::new(bus.clone());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(Recorder(seen.clone())),
        Arc::new(ToolRegistry::new()),
        Arc::new(DenyAll),
        bus,
        state,
        system_prompt(&AssistantConfig::default()),
    );
    // Stands for the tool hints and the task rules, which stay Russian.
    agent.add_prompt_extra(Arc::new(|| Some(TASK_RULES.to_string())));

    // The Russian interface, the default: nothing new in the prompt.
    assert_eq!(i18n::lang(), Lang::Ru);
    let russian_hint = answer_hint();
    assert!(cyrillic(&russian_hint));
    assert_eq!(i18n::tr("tray.quit"), "Выход");
    agent
        .handle("привет", CancellationToken::new())
        .await
        .unwrap();
    let russian = seen.lock().last().cloned().unwrap();
    assert!(russian.starts_with("Ты Мика,"), "{russian}");
    assert!(russian.ends_with(TASK_RULES), "{russian}");
    assert!(!russian.contains("Language reminder"));

    // The English interface: the settings command does these two things.
    i18n::set_lang(Lang::En);
    let mika = AssistantConfig {
        name: Lang::En.default_name().into(),
        ..Default::default()
    };
    agent.set_system_prompt(system_prompt(&mika));

    assert_eq!(i18n::lang(), Lang::En);
    assert_eq!(i18n::tr("tray.quit"), "Quit");
    assert_eq!(i18n::tr("no.such.key"), "no.such.key");
    let english_hint = answer_hint();
    assert!(!cyrillic(&english_hint), "{english_hint}");
    assert!(english_hint.contains("always allow"));
    // The automatic voice: English until the speech language is known, then
    // the language the user speaks, whatever the labels are.
    assert_eq!(i18n::voice_language(), "en");
    i18n::set_speech_language("ru");
    assert_eq!(i18n::voice_language(), "ru");
    i18n::set_speech_language("en");
    assert_eq!(i18n::voice_language(), "en");

    agent
        .handle("hello", CancellationToken::new())
        .await
        .unwrap();
    let english = seen.lock().last().cloned().unwrap();
    assert!(english.starts_with("You are Mika,"), "{english}");
    // The reminder is the last word, after the Russian rules that ask for Russian steps.
    let rules = english.find(TASK_RULES).expect("the task rules are sent");
    let note = english.find(REPLY_NOTE_EN).expect("the reminder is sent");
    assert!(rules < note);
    assert!(english.ends_with(REPLY_NOTE_EN), "{english}");

    // Back to Russian: the same agent stops sending the reminder.
    i18n::set_lang(Lang::Ru);
    agent.set_system_prompt(system_prompt(&AssistantConfig::default()));
    agent
        .handle("ещё раз", CancellationToken::new())
        .await
        .unwrap();
    let again = seen.lock().last().cloned().unwrap();
    assert!(again.starts_with("Ты Мика,"));
    assert!(!again.contains("Language reminder"));
    assert_eq!(answer_hint(), russian_hint);
    // With the Russian interface she speaks Russian, so the voice is Russian.
    assert_eq!(i18n::voice_language(), "ru");
}
