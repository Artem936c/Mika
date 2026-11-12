//! "Blind zone": windows the assistant neither looks at nor touches.
//!
//! Only the window title is checked, so this is a guard against obvious cases,
//! not a guarantee. The settings say so.

/// Parts of a title that mark a window with passwords, money or login codes.
const MARKERS: &[&str] = &[
    "парол",
    "вход в",
    "войти",
    "авториз",
    "логин",
    "банк",
    "оплат",
    "платеж",
    "платёж",
    "кошел",
    "кошёл",
    "код подтвержд",
    "двухфактор",
    "password",
    "passwort",
    "sign in",
    "signin",
    "log in",
    "login",
    "bank",
    "payment",
    "checkout",
    "wallet",
    "2fa",
    "two-factor",
    "verification code",
    "keepass",
    "1password",
    "bitwarden",
    "lastpass",
    "диспетчер учетных данных",
    "диспетчер учётных данных",
    "credential manager",
    "безопасность windows",
    "windows security",
];

/// True when the assistant must not read or control a window with this title.
pub fn is_blind_title(title: &str) -> bool {
    let t = title.to_lowercase();
    MARKERS.iter().any(|m| t.contains(m))
}

/// Parts of a web address that mark sign-in, account security, banks and payments.
const URL_MARKERS: &[&str] = &[
    "login",
    "logon",
    "signin",
    "sign-in",
    "sign_in",
    "/auth/",
    "/auth?",
    "/authorize",
    "/authenticat",
    "auth.",
    "oauth",
    "/sso",
    "sso.",
    "password",
    "passport.",
    "accounts.google.",
    "/otp",
    "2fa",
    "esia.",
    "bank",
    "payment",
    "checkout",
    "/pay",
    "pay.",
    "paypal",
    "wallet",
    "/billing",
];

/// True when the assistant must not read or control a web page at this address.
pub fn is_blind_url(url: &str) -> bool {
    let u = url.to_lowercase();
    u.ends_with("/auth") || URL_MARKERS.iter().any(|m| u.contains(m))
}

pub const BLIND_REFUSAL: &str =
    "Это окно в «слепой зоне»: в нём могут быть пароли или платёжные данные, я его не читаю и не трогаю";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_bank_and_payment_windows_are_blind() {
        for title in [
            "Вход в банк — Google Chrome",
            "Введите пароль",
            "Sign in to your account",
            "Оплата заказа №15 — Яндекс Браузер",
            "KeePass",
            "Код подтверждения",
            "Безопасность Windows",
            "MetaMask Wallet",
        ] {
            assert!(is_blind_title(title), "{title}");
        }
    }

    #[test]
    fn sign_in_and_payment_addresses_are_blind() {
        for url in [
            "https://accounts.google.com/v3/signin/identifier",
            "https://passport.yandex.ru/auth",
            "https://online.sberbank.ru/",
            "https://shop.example/checkout/step2",
            "https://example.com/login?next=/",
            "https://esia.gosuslugi.ru/",
            "https://pay.example.com/invoice/1",
        ] {
            assert!(is_blind_url(url), "{url}");
        }
        for url in [
            "https://ru.wikipedia.org/wiki/Rust",
            "https://duckduckgo.com/?q=погода",
            "https://example.com/author/ivan",
            "https://github.com/rust-lang/rust",
        ] {
            assert!(!is_blind_url(url), "{url}");
        }
    }

    #[test]
    fn ordinary_windows_are_open() {
        for title in [
            "Безымянный — Блокнот",
            "Калькулятор",
            "main.rs — Visual Studio Code",
            "Параметры",
            "",
        ] {
            assert!(!is_blind_title(title), "{title}");
        }
    }
}
