//! The list of conversations (decision 0052): open an old one and go on,
//! start a new one, rename, pin and delete, and the daily cleanup. The list
//! lives in `mika.db`; the agent holds the open one.

use std::sync::Arc;

use mika_core::storage::Conversation;

use crate::app::Ctx;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// The list and which conversation is open.
#[derive(serde::Serialize)]
pub struct ConversationsView {
    current: String,
    items: Vec<Conversation>,
}

fn view(ctx: &Ctx) -> Result<ConversationsView, String> {
    Ok(ConversationsView {
        current: ctx.session.agent.conversation().unwrap_or_default(),
        items: ctx.db.list_conversations().map_err(|e| e.to_string())?,
    })
}

impl Ctx {
    /// Starts a new conversation; the old one stays in the list. An empty
    /// open conversation is kept instead (`reuse_empty`): «+» pressed twice
    /// makes one conversation, not two.
    pub async fn new_conversation(&self, reuse_empty: bool) -> Result<(), String> {
        self.stop_for_switch();
        let agent = &self.session.agent;
        if let Some(current) = agent.conversation() {
            if reuse_empty {
                // Waits for the stopped turn to end: what it kept is counted.
                agent
                    .switch_conversation(&current)
                    .await
                    .map_err(|e| e.to_string())?;
                let empty = self
                    .db
                    .conversation(&current)
                    .map_err(|e| e.to_string())?
                    .is_some_and(|c| c.messages == 0);
                if empty {
                    return Ok(());
                }
            }
        }
        let id = self.db.create_conversation().map_err(|e| e.to_string())?;
        agent
            .switch_conversation(&id)
            .await
            .map_err(|e| e.to_string())?;
        log::info!(target: "turn", "new conversation started");
        Ok(())
    }

    /// Deletes the conversations older than the days of the «Память» page,
    /// but for the pinned ones, the open one and one opened lately. At start
    /// and once a day. Blocking: SQLite and the disk; run it off the async
    /// threads.
    pub fn purge_conversations(&self) {
        self.purge_old_conversations();
        self.sweep_waiting_files();
        // Folders of attached files whose conversation went another way, such
        // as a restore of the history from a copy (decision 0053).
        match self
            .db
            .prune_attachments(std::time::Duration::from_secs(86_400))
        {
            Ok(0) => {}
            Ok(n) => log::info!(
                target: "app",
                "attachment folders of gone conversations removed: count={n}"
            ),
            Err(e) => log::warn!(target: "app", "cannot look over the attachment folders: {e}"),
        }
    }

    fn purge_old_conversations(&self) {
        let Some(days) = self.config.lock().history.keep() else {
            return;
        };
        let Some(keep) = self.session.agent.conversation() else {
            return;
        };
        match self.db.purge_older_than(days, &keep) {
            Ok(n) => log::info!(
                target: "app",
                "old conversations deleted: count={n} older_than_days={days}"
            ),
            Err(e) => log::warn!(target: "app", "cannot delete old conversations: {e}"),
        }
    }
}

#[tauri::command]
pub fn list_conversations(ctx: CtxState<'_>) -> Result<ConversationsView, String> {
    view(&ctx)
}

/// Opens a conversation of the list: a running reply or task stops first.
/// The chat then reads `get_history`.
#[tauri::command]
pub async fn open_conversation(ctx: CtxState<'_>, id: String) -> Result<ConversationsView, String> {
    if ctx.session.agent.conversation().as_deref() == Some(id.as_str()) {
        return view(&ctx);
    }
    if ctx
        .db
        .conversation(&id)
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Err(crate::strings::tr("error.conversation_gone"));
    }
    ctx.stop_for_switch();
    ctx.session
        .agent
        .switch_conversation(&id)
        .await
        .map_err(|e| e.to_string())?;
    log::info!(target: "turn", "conversation opened from the list");
    view(&ctx)
}

#[tauri::command]
pub async fn new_conversation(ctx: CtxState<'_>) -> Result<ConversationsView, String> {
    ctx.new_conversation(true).await?;
    view(&ctx)
}

/// An empty title gives back the one from the first message.
#[tauri::command]
pub fn rename_conversation(
    ctx: CtxState<'_>,
    id: String,
    title: String,
) -> Result<ConversationsView, String> {
    if !ctx
        .db
        .rename_conversation(&id, &title)
        .map_err(|e| e.to_string())?
    {
        return Err(crate::strings::tr("error.conversation_gone"));
    }
    view(&ctx)
}

#[tauri::command]
pub fn pin_conversation(
    ctx: CtxState<'_>,
    id: String,
    pinned: bool,
) -> Result<ConversationsView, String> {
    if !ctx
        .db
        .pin_conversation(&id, pinned)
        .map_err(|e| e.to_string())?
    {
        return Err(crate::strings::tr("error.conversation_gone"));
    }
    view(&ctx)
}

/// Deletes a conversation with its messages. Deleting the open one starts a
/// new one first; the chat sees `current` change and reads the history again.
#[tauri::command]
pub async fn delete_conversation(
    ctx: CtxState<'_>,
    id: String,
) -> Result<ConversationsView, String> {
    if ctx.session.agent.conversation().as_deref() == Some(id.as_str()) {
        ctx.new_conversation(false).await?;
    }
    let n = ctx.db.delete_conversation(&id).map_err(|e| e.to_string())?;
    log::info!(target: "app", "conversation deleted: messages={n}");
    view(&ctx)
}

/// The «Память» page: deletes every conversation that is not pinned, but for
/// the open one. Returns how many went.
#[tauri::command]
pub fn delete_unpinned_conversations(ctx: CtxState<'_>) -> Result<usize, String> {
    let keep = ctx.session.agent.conversation().unwrap_or_default();
    let n = ctx.db.delete_unpinned(&keep).map_err(|e| e.to_string())?;
    log::info!(target: "app", "conversations deleted from the memory page: count={n}");
    Ok(n)
}

/// «Удалять диалоги старше N дней»: 7, 30, 90, 365, or 0 for «никогда».
#[tauri::command]
pub fn set_history_keep(ctx: CtxState<'_>, days: u32) -> Result<(), String> {
    if !mika_core::config::HistoryConfig::CHOICES.contains(&days) {
        return Err(format!("unknown choice: {days}"));
    }
    ctx.config.lock().history.keep_days = days;
    ctx.save_config()
}
