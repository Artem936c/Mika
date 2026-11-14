//! Which pages of the search results are read. Code decides, not the model,
//! and only addresses from the results are ever fetched.

use std::collections::{HashMap, HashSet};

use super::net::{check_url, NetRefusal};
use super::page::{one_line, TITLE_CHARS};

/// Results taken from one query.
pub const RESULTS_PER_QUERY: usize = 8;
/// Pages read from one site.
pub const PER_HOST: usize = 2;

const BLIND_REASON: &str = "страница входа, банка или оплаты";
const FILE_REASON: &str = "файл, а не страница";

/// Endings of addresses that are files, not pages.
const FILE_ENDINGS: [&str; 30] = [
    ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".odt", ".rtf", ".zip", ".rar",
    ".7z", ".gz", ".exe", ".msi", ".apk", ".dmg", ".iso", ".mp3", ".mp4", ".avi", ".mkv", ".mov",
    ".wav", ".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg",
];

/// Query parameters that only track the visitor.
const TRACKING: [&str; 5] = ["fbclid", "gclid", "yclid", "ysclid", "_openstat"];

/// A page to read.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub url: String,
    /// The title from the search results, for a page that has none.
    pub title: String,
    pub host: String,
}

/// A page that was not read or gave nothing, with the reason for the report.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    pub url: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Picked {
    pub candidates: Vec<Candidate>,
    pub skipped: Vec<Skipped>,
    /// Addresses the network check refused; the report names only their number.
    pub refused: usize,
}

fn is_tracking(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_") || TRACKING.contains(&key.as_str())
}

/// Same page, same string: no fragment, no tracking parameters, lower-case host.
pub fn normalize_url(url: &reqwest::Url) -> String {
    let mut clean = url.clone();
    clean.set_fragment(None);
    if let Some(query) = url.query() {
        // The parts are kept as they are written: decoding and writing them
        // again could change what the site reads.
        let kept: Vec<&str> = query
            .split('&')
            .filter(|part| !part.is_empty() && !is_tracking(part.split('=').next().unwrap_or("")))
            .collect();
        if kept.is_empty() {
            clean.set_query(None);
        } else {
            clean.set_query(Some(&kept.join("&")));
        }
    }
    clean.to_string()
}

/// One key for the addresses that lead to the same page: the normalized
/// address without its scheme and without a leading `www.` of the host.
pub fn page_key(url: &reqwest::Url) -> String {
    let normalized = normalize_url(url);
    let rest = normalized
        .split_once("://")
        .map_or(normalized.as_str(), |(_, rest)| rest);
    rest.strip_prefix("www.").unwrap_or(rest).to_string()
}

fn is_search_engine(host: &str) -> bool {
    host == "duckduckgo.com" || host.ends_with(".duckduckgo.com")
}

fn is_file(url: &reqwest::Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    FILE_ENDINGS.iter().any(|e| path.ends_with(e))
}

/// Pages to read, best first: results taken in turn from every query, by rank.
/// Each result is `(title, url, snippet)`. The snippet is never used: it is
/// text the site owner controls.
pub fn pick(results: &[Vec<(String, String, String)>], limit: usize) -> Picked {
    let mut picked = Picked::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut per_host: HashMap<String, usize> = HashMap::new();
    for rank in 0..RESULTS_PER_QUERY {
        for list in results {
            if picked.candidates.len() >= limit {
                return picked;
            }
            let Some((title, raw, _snippet)) = list.get(rank) else {
                continue;
            };
            // The search page writes some of its own links without a scheme.
            let raw = raw.trim();
            let address = match raw.strip_prefix("//") {
                Some(rest) => format!("https://{rest}"),
                None => raw.to_string(),
            };
            let (url, blind) = match check_url(&address) {
                Ok(url) => (url, false),
                Err(NetRefusal::Blind) => match reqwest::Url::parse(&address) {
                    Ok(url) => (url, true),
                    Err(_) => {
                        picked.refused += 1;
                        continue;
                    }
                },
                Err(_) => {
                    picked.refused += 1;
                    continue;
                }
            };
            let host = url.host_str().unwrap_or("").to_lowercase();
            if is_search_engine(&host) {
                continue;
            }
            let normalized = normalize_url(&url);
            // `http://site/a`, `https://site/a` and `https://www.site/a` are
            // one page: it is read once.
            if !seen.insert(page_key(&url)) {
                continue;
            }
            if blind {
                picked.skipped.push(Skipped {
                    url: normalized,
                    reason: BLIND_REASON.into(),
                });
                continue;
            }
            if is_file(&url) {
                picked.skipped.push(Skipped {
                    url: normalized,
                    reason: FILE_REASON.into(),
                });
                continue;
            }
            let site = host.strip_prefix("www.").unwrap_or(&host).to_string();
            let taken = per_host.entry(site).or_insert(0);
            if *taken >= PER_HOST {
                continue;
            }
            *taken += 1;
            picked.candidates.push(Candidate {
                url: normalized,
                title: one_line(title, TITLE_CHARS),
                host,
            });
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(title: &str, url: &str) -> (String, String, String) {
        (title.into(), url.into(), "описание от сайта".into())
    }

    fn urls(picked: &Picked) -> Vec<&str> {
        picked.candidates.iter().map(|c| c.url.as_str()).collect()
    }

    #[test]
    fn same_page_gets_the_same_string() {
        let norm = |u: &str| normalize_url(&reqwest::Url::parse(u).unwrap());
        assert_eq!(
            norm("https://Example.COM/a?utm_source=x&id=7&UTM_Medium=y&fbclid=1#part"),
            "https://example.com/a?id=7"
        );
        assert_eq!(
            norm("https://example.com/a?gclid=1&yclid=2&ysclid=3&_openstat=4"),
            "https://example.com/a"
        );
        assert_eq!(norm("https://example.com/a#x"), "https://example.com/a");
        // What is not tracking stays as it is written.
        assert_eq!(
            norm("https://example.com/s?q=a%20b+c&page"),
            "https://example.com/s?q=a%20b+c&page"
        );
        assert_eq!(norm("https://example.com"), "https://example.com/");
    }

    #[test]
    fn scheme_and_www_do_not_make_another_page() {
        let key = |u: &str| page_key(&reqwest::Url::parse(u).unwrap());
        assert_eq!(key("https://www.Site.ru/a?utm_source=x#top"), "site.ru/a");
        assert_eq!(key("http://site.ru/a"), "site.ru/a");
        assert_eq!(key("https://site.ru/a?id=7"), "site.ru/a?id=7");
        // Another port, another path and another site are other pages.
        assert_eq!(key("http://site.ru:8080/a"), "site.ru:8080/a");
        assert_ne!(key("https://site.ru/a/"), key("https://site.ru/a"));
        assert_ne!(key("https://blog.site.ru/a"), key("https://site.ru/a"));
        assert_eq!(key("https://wwwsite.ru/"), "wwwsite.ru/");
    }

    #[test]
    fn results_are_taken_in_turn_by_rank() {
        let results = vec![
            vec![
                hit("A1", "https://a.example.org/1"),
                hit("A2", "https://b.example.org/2"),
                hit("A3", "https://c.example.org/3"),
            ],
            vec![
                hit("B1", "https://d.example.org/1"),
                hit("B2", "https://e.example.org/2"),
            ],
        ];
        let picked = pick(&results, 10);
        assert_eq!(
            urls(&picked),
            vec![
                "https://a.example.org/1",
                "https://d.example.org/1",
                "https://b.example.org/2",
                "https://e.example.org/2",
                "https://c.example.org/3",
            ]
        );
        assert_eq!(picked.candidates[0].title, "A1");
        assert_eq!(picked.candidates[0].host, "a.example.org");
        assert!(picked.skipped.is_empty() && picked.refused == 0);
        // The limit cuts the list where it stands.
        assert_eq!(urls(&pick(&results, 3)).len(), 3);
        assert_eq!(urls(&pick(&results, 3))[2], "https://b.example.org/2");
        assert!(pick(&results, 0).candidates.is_empty());
        assert!(pick(&[], 5).candidates.is_empty());
    }

    #[test]
    fn two_pages_a_site_and_no_page_twice() {
        let results = vec![vec![
            hit("1", "https://www.site.org/a"),
            hit("2", "https://site.org/b?utm_campaign=x"),
            hit("3", "https://site.org/c"),
            hit("4", "https://WWW.site.org/a#top"),
            hit("5", "https://other.org/a?utm_source=ddg"),
            hit("6", "https://other.org/a"),
            hit("7", "https://blog.site.org/d"),
            hit("8", "https://site.org/a"),
            hit("9", "http://other.org/a"),
            hit("10", "http://www.other.org/a?utm_term=y#x"),
        ]];
        let picked = pick(&results, 10);
        assert_eq!(
            urls(&picked),
            vec![
                "https://www.site.org/a",
                "https://site.org/b",
                "https://other.org/a",
                "https://blog.site.org/d",
            ]
        );
    }

    #[test]
    fn only_eight_results_of_a_query_are_looked_at() {
        let list: Vec<_> = (0..12)
            .map(|i| hit("t", &format!("https://s{i}.example.org/")))
            .collect();
        assert_eq!(pick(&[list], 50).candidates.len(), RESULTS_PER_QUERY);
    }

    #[test]
    fn unsafe_and_useless_results_never_become_candidates() {
        let results = vec![vec![
            hit("Реклама", "//duckduckgo.com/y.js?ad_provider=x"),
            hit("Ещё", "https://duckduckgo.com/?q=more"),
            hit("Роутер", "http://192.168.0.1/admin"),
            hit("Локальный", "http://127.0.0.1/"),
            hit("Файл", "file:///C:/Users/me/secret.txt"),
            hit("Порт", "https://example.org:8081/"),
            hit("Вход", "https://bank.example.org/login?next=/"),
            hit("Отчёт", "https://example.org/files/Report.PDF?download=1"),
        ]];
        let picked = pick(&results, 10);
        assert!(picked.candidates.is_empty(), "{:?}", picked.candidates);
        assert_eq!(picked.refused, 4);
        assert_eq!(
            picked.skipped,
            vec![
                Skipped {
                    url: "https://bank.example.org/login?next=/".into(),
                    reason: "страница входа, банка или оплаты".into()
                },
                Skipped {
                    url: "https://example.org/files/Report.PDF?download=1".into(),
                    reason: "файл, а не страница".into()
                },
            ]
        );
    }

    #[test]
    fn a_skipped_page_is_listed_once() {
        let results = vec![
            vec![hit("Вход", "https://example.org/login")],
            vec![hit("Вход", "https://example.org/login#form")],
            vec![hit("Файл", "https://example.org/a.zip")],
            vec![hit("Файл", "https://example.org/a.zip?utm_source=x")],
        ];
        let picked = pick(&results, 10);
        assert_eq!(picked.skipped.len(), 2);
    }

    #[test]
    fn title_from_the_results_is_one_short_line() {
        let long = format!("Заголовок\n с переносом {}", "ж".repeat(300));
        let picked = pick(&[vec![hit(&long, "https://example.org/")]], 5);
        let title = &picked.candidates[0].title;
        assert!(title.starts_with("Заголовок с переносом"));
        assert_eq!(title.chars().count(), TITLE_CHARS);
    }
}
