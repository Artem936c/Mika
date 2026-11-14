//! The first step: the model names the search queries.

use super::page::{one_line, strip_bullet};

/// Characters of the plan answer that are read.
pub const PLAN_CHARS: usize = 800;
const QUERY_MIN: usize = 3;
const QUERY_MAX: usize = 150;

/// What the model is told before it names the queries. `today` is `%d.%m.%Y`.
pub fn plan_system(max_queries: u32, today: &str) -> String {
    format!(
        "Ты готовишь поиск в интернете по вопросу пользователя. Напиши от 2 до {max_queries} \
поисковых запросов, которые вместе покрывают вопрос с разных сторон: определения, сравнение, \
цифры, свежие данные. Один запрос на строку. Без нумерации, кавычек и пояснений. Запросы \
короткие, как их набирают в поисковике, на языке вопроса; если тема иностранная или \
техническая, один запрос напиши по-английски. Сегодня {today}."
    )
}

pub fn plan_user(question: &str) -> String {
    format!("Вопрос: {question}")
}

/// Search queries from the model's answer; the question itself when none can
/// be read.
pub fn parse_queries(answer: &str, max: usize, question: &str) -> Vec<String> {
    let mut queries: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for line in answer.lines() {
        let line = strip_bullet(line).unwrap_or(line);
        let query = one_line(
            line.trim()
                .trim_matches(['"', '\'', '«', '»', '“', '”', '„'])
                .trim(),
            usize::MAX,
        );
        let len = query.chars().count();
        if !(QUERY_MIN..=QUERY_MAX).contains(&len) || query.ends_with(':') {
            continue;
        }
        let key = query.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        queries.push(query);
        if queries.len() >= max {
            break;
        }
    }
    if queries.is_empty() {
        queries.push(one_line(question, QUERY_MAX));
    }
    queries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_names_the_limit_and_the_day() {
        let p = plan_system(5, "14.11.2026");
        assert!(p.contains("от 2 до 5 поисковых запросов"), "{p}");
        assert!(p.ends_with("Сегодня 14.11.2026."), "{p}");
        assert!(p.contains("Один запрос на строку."));
        assert_eq!(plan_user("что выгоднее?"), "Вопрос: что выгоднее?");
    }

    #[test]
    fn queries_are_read_from_lines() {
        let answer = "тепловой насос воздух-вода отзывы\nгрунтовый тепловой насос цена\n\nheat pump COP comparison";
        assert_eq!(
            parse_queries(answer, 5, "вопрос"),
            vec![
                "тепловой насос воздух-вода отзывы",
                "грунтовый тепловой насос цена",
                "heat pump COP comparison"
            ]
        );
    }

    #[test]
    fn numbering_quotes_and_duplicates_are_dropped() {
        let answer = "Вот запросы:\n1. «тепловой насос цена»\n2) \"Тепловой насос ЦЕНА\"\n- 'heat pump cost'\n• “грунт-вода монтаж”\n* ок\n— воздух-вода  в   мороз\nТема:";
        assert_eq!(
            parse_queries(answer, 10, "вопрос"),
            vec![
                "тепловой насос цена",
                "heat pump cost",
                "грунт-вода монтаж",
                "воздух-вода в мороз"
            ]
        );
    }

    #[test]
    fn queries_stop_at_the_limit_and_long_lines_are_skipped() {
        let long = "длинный ".repeat(30);
        let answer = format!("{long}\nпервый запрос\nвторой запрос\nтретий запрос");
        assert_eq!(
            parse_queries(&answer, 2, "вопрос"),
            vec!["первый запрос", "второй запрос"]
        );
    }

    #[test]
    fn question_is_the_query_when_the_answer_gives_none() {
        assert_eq!(
            parse_queries("", 5, "  Чем насос\nлучше котла?  "),
            vec!["Чем насос лучше котла?"]
        );
        assert_eq!(parse_queries("ок\n:\n-", 5, "вопрос"), vec!["вопрос"]);
        let long = "слово ".repeat(100);
        assert_eq!(parse_queries("", 5, &long)[0].chars().count(), 149);
    }
}
