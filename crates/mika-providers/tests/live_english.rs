//! The reply language with the English prompt, on the user's own model.
//! Reads the settings and the key of the installed Mika and makes two paid requests.
//!
//! `cargo test -p mika-providers live_english_reply -- --ignored --nocapture`

use std::time::Duration;

use mika_core::config::{AssistantConfig, Config};
use mika_core::i18n::prompt::{system_prompt_en, REPLY_NOTE_EN};
use mika_core::paths::config_path;
use mika_core::secrets::KeyringSecretStore;

fn cyrillic(text: &str) -> bool {
    text.chars().any(|c| ('\u{0400}'..='\u{04ff}').contains(&c))
}

/// The English base with the Russian task rules, as the agent sends them: an
/// English question gets an English reply, a Russian one a Russian reply.
#[tokio::test]
#[ignore = "calls the LLM provider: paid"]
async fn live_english_reply() {
    let config = Config::load(&config_path())
        .unwrap_or_else(|e| panic!("the settings of Mika do not load: {e}"));
    let secrets = KeyringSecretStore::new();
    let provider = mika_providers::build_or_placeholder(&config.llm, &secrets);
    let assistant = AssistantConfig {
        name: "Mika".into(),
        ..Default::default()
    };
    let system = format!(
        "{}\n\n{}\n\n{}",
        system_prompt_en(&assistant),
        mika_core::task::TASK_RULES,
        REPLY_NOTE_EN
    );
    let ask = |question: &'static str| {
        let (provider, system) = (provider.clone(), system.clone());
        async move {
            mika_core::llm::complete(
                provider.as_ref(),
                &system,
                question,
                Duration::from_secs(30),
            )
            .await
        }
    };

    let english = ask("What can you do for me?")
        .await
        .expect("a reply to the English question");
    println!("English question: {english}");
    assert!(
        !cyrillic(&english),
        "an English question got Cyrillic letters in the reply"
    );

    let russian = ask("Что ты умеешь?")
        .await
        .expect("a reply to the Russian question");
    println!("Russian question: {russian}");
    assert!(
        cyrillic(&russian),
        "a Russian question got a reply without Cyrillic letters"
    );
}
