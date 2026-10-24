//! Turns the engine's raw transcript into the text that gets pasted: filler words
//! out, stutters collapsed, then spoken commands ("open paren", "new line") replaced
//! by their symbols with the spacing a person would type.

use crate::settings::Rule;

pub struct Options {
    pub remove_fillers: bool,
    pub collapse_repeats: bool,
}

pub fn process(raw: &str, rules: &[Rule], opts: &Options) -> String {
    let mut tokens: Vec<String> = raw.split_whitespace().map(String::from).collect();
    if opts.remove_fillers {
        tokens = remove_fillers(tokens);
    }
    if opts.collapse_repeats {
        tokens = collapse_repeats(tokens);
    }
    tidy(&apply_rules(&tokens.join(" "), rules))
}

// ---------------------------------------------------------------- fillers

const FILLERS: &[&str] = &["um", "umm", "uhm", "uh", "uhh", "erm", "hm", "hmm", "hmmm", "mm", "mmm"];
const SENTENCE_END: [char; 3] = ['.', '?', '!'];

/// Lowercase word with surrounding punctuation stripped.
fn core(token: &str) -> String {
    token.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

fn is_filler(token: &str) -> bool {
    FILLERS.contains(&core(token).as_str())
}

fn capitalize(token: &str) -> String {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn starts_upper(token: &str) -> bool {
    token.chars().find(|c| c.is_alphabetic()).is_some_and(char::is_uppercase)
}

fn remove_fillers(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut capitalize_next = false;
    for token in tokens {
        if is_filler(&token) {
            // "It was, uh." keeps its full stop: move it onto the previous word.
            if let Some(end) = token.chars().last().filter(|c| SENTENCE_END.contains(c))
                && let Some(prev) = out.last_mut()
            {
                if prev.ends_with([',', ';', ':']) {
                    prev.pop();
                }
                if !prev.ends_with(SENTENCE_END) {
                    prev.push(end);
                }
            }
            // "Uh, this number…" → "This number…"
            if starts_upper(&token) {
                capitalize_next = true;
            }
            continue;
        }
        out.push(if std::mem::take(&mut capitalize_next) { capitalize(&token) } else { token });
    }
    out
}

// ---------------------------------------------------------------- repeats

/// Single-word doubles that are usually intended ("I know that that works").
const KEEP_DOUBLES: &[&str] = &["that", "had", "is", "do", "bye", "no", "yeah"];

/// "the the", "at the at the", "and, and now" → one copy. A full stop between the
/// copies ("Yes. Yes.") marks two sentences, so those are left alone.
fn collapse_repeats(mut tokens: Vec<String>) -> Vec<String> {
    let mut i = 0;
    while i < tokens.len() {
        let mut collapsed = false;
        for n in (1..=3).rev() {
            if i + 2 * n > tokens.len() {
                continue;
            }
            let first: Vec<String> = tokens[i..i + n].iter().map(|t| core(t)).collect();
            let second: Vec<String> = tokens[i + n..i + 2 * n].iter().map(|t| core(t)).collect();
            let boundary = &tokens[i + n - 1];
            let same = first == second && first.iter().all(|w| !w.is_empty());
            let numeric = first.iter().any(|w| w.chars().any(|c| c.is_ascii_digit()));
            let keep = n == 1 && KEEP_DOUBLES.contains(&first[0].as_str());
            if same && !numeric && !keep && !boundary.ends_with(SENTENCE_END) {
                let was_upper = starts_upper(&tokens[i]);
                tokens.drain(i..i + n);
                if was_upper {
                    tokens[i] = capitalize(&tokens[i]);
                }
                collapsed = true;
                break;
            }
        }
        if !collapsed {
            i += 1;
        }
    }
    tokens
}

// ---------------------------------------------------------------- spoken commands

// Replacements are wrapped as MARK, kind, text, END so `tidy` can fix the spacing
// around them afterwards without touching anything the model wrote itself.
const MARK: char = '\u{E000}';
const END: char = '\u{E001}';

/// How a replacement joins its neighbours.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Join {
    /// "(" "$" "@" and opening quotes: no space after.
    Right,
    /// ")" "," "%" and closing quotes: no space before.
    Left,
    /// Line breaks: no spaces on either side, capital letter after.
    Line,
    /// Anything else keeps the spaces as spoken.
    Keep,
}

fn join_kind(spoken: &str, replacement: &str) -> Join {
    if replacement.contains('\n') {
        return Join::Line;
    }
    let s = spoken.to_ascii_lowercase();
    if ["open ", "left ", "begin "].iter().any(|p| s.starts_with(p)) {
        return Join::Right;
    }
    if ["close ", "right ", "end "].iter().any(|p| s.starts_with(p)) {
        return Join::Left;
    }
    match replacement.trim() {
        "(" | "[" | "{" | "$" | "#" | "@" | "€" | "£" | "¥" => Join::Right,
        ")" | "]" | "}" | "," | "." | "?" | "!" | ";" | ":" | "..." | "…" | "%" | "°" | "¢" | "™" | "®" => Join::Left,
        _ if replacement.starts_with(' ') => Join::Left, // " :)" carries its own space
        _ => Join::Keep,
    }
}

fn apply_rules(text: &str, rules: &[Rule]) -> String {
    let mut sorted: Vec<&Rule> = rules.iter().filter(|r| !r.spoken.trim().is_empty()).collect();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.spoken.len()));
    let mut out = text.to_string();
    for rule in sorted {
        let tag = match join_kind(&rule.spoken, &rule.replacement) {
            Join::Right => 'R',
            Join::Left => 'L',
            Join::Line => 'N',
            Join::Keep => 'K',
        };
        let wrapped = format!("{MARK}{tag}{}{END}", rule.replacement);
        let symbol = !rule.replacement.chars().any(char::is_alphanumeric);
        out = replace_phrase(&out, rule.spoken.trim(), &wrapped, symbol);
    }
    out
}

fn is_word_char(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '\'')
}

/// Case-insensitive, whole-word replacement. With `absorb`, a comma or full stop
/// the model put right after a spoken symbol is dropped ("open paren," → "(").
fn replace_phrase(text: &str, phrase: &str, replacement: &str, absorb: bool) -> String {
    // ASCII lowercasing keeps byte offsets identical, so indexes into `lower` are valid in `text`.
    let lower = text.to_ascii_lowercase();
    let needle = phrase.to_ascii_lowercase();
    let needs_left_edge = needle.starts_with(|c: char| c.is_alphanumeric());
    let needs_right_edge = needle.ends_with(|c: char| c.is_alphanumeric());
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(pos) = lower[i..].find(&needle) {
        let start = i + pos;
        let end = start + needle.len();
        let left_ok = !needs_left_edge || !is_word_char(text[..start].chars().next_back());
        let right_ok = !needs_right_edge || !is_word_char(text[end..].chars().next());
        if left_ok && right_ok {
            out.push_str(&text[i..start]);
            out.push_str(replacement);
            i = end;
            if absorb && text[i..].starts_with([',', '.']) && !text[i..].starts_with("...") {
                i += 1;
            }
        } else {
            let step = text[start..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&text[i..start + step]);
            i = start + step;
        }
    }
    out.push_str(&text[i..]);
    out
}

/// Resolve the replacement markers into final spacing, then tidy whitespace.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    let mut skip_spaces = false;
    let mut capitalize_next = false;
    while let Some(c) = chars.next() {
        if c == MARK {
            let tag = chars.next().unwrap_or('K');
            let body: String = chars.by_ref().take_while(|&c| c != END).collect();
            if tag == 'L' || tag == 'N' {
                while out.ends_with([' ', '\t']) {
                    out.pop();
                }
                // "bar, close paren" → "bar)", not "bar,)".
                if matches!(body.trim(), ")" | "]" | "}" | "\"" | "'") && out.ends_with(',') {
                    out.pop();
                }
            }
            out.push_str(&body);
            skip_spaces = tag == 'R' || tag == 'N';
            capitalize_next |= tag == 'N';
            continue;
        }
        if skip_spaces && (c == ' ' || c == '\t') {
            continue;
        }
        skip_spaces = false;
        if capitalize_next && c.is_alphanumeric() {
            out.extend(c.to_uppercase());
            capitalize_next = false;
            continue;
        }
        out.push(c);
    }
    // Collapse runs of spaces (not line breaks) and trim each line's edges.
    let lines: Vec<String> = out
        .split('\n')
        .map(|line| line.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" "))
        .collect();
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<Rule> {
        crate::settings::Settings::default().find_replace
    }

    fn run(raw: &str) -> String {
        process(raw, &rules(), &Options { remove_fillers: true, collapse_repeats: true })
    }

    #[test]
    fn removes_fillers_and_fixes_case() {
        assert_eq!(
            run("Uh, this number is 250 grams, uh, so 450,000 tons."),
            "This number is 250 grams, so 450,000 tons."
        );
        assert_eq!(run("It was, uh."), "It was.");
        assert_eq!(run("so um the plan is fine"), "so the plan is fine");
    }

    #[test]
    fn collapses_stutters() {
        assert_eq!(
            run("at the at the end of 2019 and and now it uh it is in full effect"),
            "at the end of 2019 and now it is in full effect"
        );
        assert_eq!(run("And, and now we ship"), "And now we ship");
        assert_eq!(run("I I think so"), "I think so");
    }

    #[test]
    fn keeps_intended_repeats() {
        assert_eq!(run("I know that that is true"), "I know that that is true");
        assert_eq!(run("Yes. Yes. Do it."), "Yes. Yes. Do it.");
        assert_eq!(run("call 555 555 1234"), "call 555 555 1234");
    }

    #[test]
    fn brackets_and_quotes() {
        assert_eq!(run("call foo open paren bar close paren now"), "call foo (bar) now");
        assert_eq!(run("Call foo, open paren, bar, close paren."), "Call foo, (bar)");
        assert_eq!(run("type open quote hello close quote please"), "type \"hello\" please");
        assert_eq!(run("open bracket one close bracket"), "[one]");
    }

    #[test]
    fn line_breaks() {
        assert_eq!(run("first line new line second line"), "first line\nSecond line");
        assert_eq!(
            run("Dear Sam, new paragraph. thanks for the update."),
            "Dear Sam,\n\nThanks for the update."
        );
    }

    #[test]
    fn punctuation_and_symbols() {
        assert_eq!(run("a comma b"), "a, b");
        assert_eq!(run("the commander said no"), "the commander said no");
        assert_eq!(run("growth of 50 percent sign"), "growth of 50%");
        assert_eq!(run("ping me at sign piedol"), "ping me @piedol");
        assert_eq!(run("is it done question mark"), "is it done?");
    }

    #[test]
    fn options_off_leave_text_alone() {
        let out = process("uh the the plan", &[], &Options { remove_fillers: false, collapse_repeats: false });
        assert_eq!(out, "uh the the plan");
    }
}
