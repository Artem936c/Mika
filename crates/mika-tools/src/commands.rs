//! User commands: named instructions the user can call by name.

use async_trait::async_trait;
use mika_core::commands::CommandStore;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::util::{failed, schema, truncate};

pub struct CommandSaveTool(pub CommandStore);

#[async_trait]
impl Tool for CommandSaveTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "command_save",
            "Сохранить свою команду пользователя: короткое имя и что по нему делать. Потом пользователь пишет только имя, и ты выполняешь шаги. Пример: name «рабочий режим», prompt «Открой Блокнот, калькулятор и сайт почты».",
            schema(
                &[
                    ("name", "string", "Имя команды, как его будет писать пользователь"),
                    ("prompt", "string", "Что делать по этой команде, по шагам"),
                ],
                &["name", "prompt"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let name = arg_str(args, "name")?.trim();
        let prompt = arg_str(args, "prompt")?;
        Ok(Assessment::new(
            RiskLevel::Medium,
            format!("Сохранить команду «{name}»"),
            truncate(prompt, 300),
            "команда будет выполняться по одному слову; её действия всё равно спросят разрешение",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let name = arg_str(&args, "name")?.trim();
        self.0
            .save(name, arg_str(&args, "prompt")?)
            .map_err(|e| failed(e.to_string()))?;
        Ok(ToolOutput::new(format!(
            "Команда «{name}» сохранена. Чтобы её выполнить, достаточно написать её имя."
        )))
    }
}

pub struct CommandListTool(pub CommandStore);

#[async_trait]
impl Tool for CommandListTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "command_list",
            "Показать сохранённые команды пользователя.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть свои команды",
            "команды",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let list = self.0.list().map_err(|e| failed(e.to_string()))?;
        if list.is_empty() {
            return Ok(ToolOutput::new("Своих команд пока нет."));
        }
        let lines: Vec<String> = list
            .iter()
            .map(|c| format!("«{}»: {}", c.name, truncate(&c.prompt, 200)))
            .collect();
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

pub struct CommandDeleteTool(pub CommandStore);

#[async_trait]
impl Tool for CommandDeleteTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "command_delete",
            "Удалить сохранённую команду по имени.",
            schema(&[("name", "string", "Имя команды")], &["name"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let name = arg_str(args, "name")?.trim();
        Ok(Assessment::new(
            RiskLevel::Low,
            format!("Удалить команду «{name}»"),
            name,
            "команду можно сохранить снова",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let name = arg_str(&args, "name")?.trim();
        if self.0.delete(name).map_err(|e| failed(e.to_string()))? {
            Ok(ToolOutput::new(format!("Команда «{name}» удалена.")))
        } else {
            Err(failed(format!("нет команды «{name}»")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use mika_core::storage::Db;
    use std::sync::Arc;

    #[tokio::test]
    async fn save_list_delete() {
        let s = CommandStore::new(Arc::new(Db::open_in_memory().unwrap()));
        run(
            &CommandSaveTool(s.clone()),
            json!({"name": "Рабочий режим", "prompt": "Открой Блокнот и калькулятор"}),
        )
        .await
        .unwrap();
        assert_eq!(
            run(&CommandListTool(s.clone()), json!({}))
                .await
                .unwrap()
                .text,
            "«Рабочий режим»: Открой Блокнот и калькулятор"
        );
        assert!(s.find("рабочий режим").is_some());
        run(
            &CommandDeleteTool(s.clone()),
            json!({"name": "рабочий режим"}),
        )
        .await
        .unwrap();
        assert!(run(
            &CommandDeleteTool(s.clone()),
            json!({"name": "рабочий режим"})
        )
        .await
        .is_err());
        assert_eq!(
            run(&CommandListTool(s.clone()), json!({}))
                .await
                .unwrap()
                .text,
            "Своих команд пока нет."
        );
        assert!(
            run(&CommandSaveTool(s), json!({"name": "?", "prompt": "x"}))
                .await
                .is_err()
        );
    }
}
