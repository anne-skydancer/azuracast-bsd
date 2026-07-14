//! Parser for the `annotate:key="val",key2="val2",...:path` URI syntax that
//! `nextsong` (`engine/SPEC.md` D.1) hands back to the engine, and that
//! `azuracast.add_fallback` (C.8) also uses for the local fallback file.
//!
//! This is the *inverse* operation of PHP's `ConfigWriter::annotateValue`/
//! `annotateArray` (SPEC.md B.15), which is what originally builds these
//! strings: each value is emitted as a double-quoted string with embedded
//! `"` escaped to `\"`, and embedded newlines/tabs/carriage returns already
//! stripped out *before* being embedded (i.e. the writer never emits a raw
//! `\n`/`\t`/`\r` inside a value at all) — so the only escape sequence this
//! parser needs to undo is `\"` -> `"`. No other backslash escapes exist in
//! this grammar (a literal backslash not followed by `"` is passed through
//! unchanged).

use std::collections::HashMap;

/// The parsed form of a `nextsong`/fallback URI: zero or more `key=value`
/// annotations, plus the bare path/URI they were describing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Annotations {
    values: HashMap<String, String>,
    pub path: String,
}

impl Annotations {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }

    /// Parses `"true"`/`"false"` (the literal strings PHP's annotateArray
    /// emits for bool-typed annotations) into a real bool. Anything else
    /// (including absence of the key) is `false`.
    pub fn get_bool(&self, key: &str) -> bool {
        self.get(key) == Some("true")
    }

    /// Parses a numeric annotation value as `f64`. Returns `None` if the key
    /// is absent or not parseable as a float.
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(|v| v.trim().parse::<f64>().ok())
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }
}

/// Parses a `nextsong`-style URI. If `uri` doesn't start with the
/// `annotate:` prefix at all, returns an empty annotation map with `path`
/// set to the whole input string unchanged.
///
/// Parsing is a small hand-written recursive-descent-ish scan rather than a
/// regex, because values are free-form text (titles/artists can contain
/// commas, colons, `=`, etc.) and the only reliable way to find the
/// pairs/path boundary is to track quote state character-by-character, the
/// same way Liquidsoap's own `string.annotate.parse` grammar does.
pub fn parse_annotated_uri(uri: &str) -> Annotations {
    let rest = match uri.strip_prefix("annotate:") {
        Some(r) => r,
        None => {
            return Annotations {
                values: HashMap::new(),
                path: uri.to_string(),
            }
        }
    };

    let mut values = HashMap::new();
    let chars: Vec<char> = rest.chars().collect();
    let n = chars.len();
    let mut i = 0usize;

    loop {
        let key_start = i;
        while i < n && chars[i] != '=' {
            i += 1;
        }
        if i >= n {
            // No '=' found before the string ran out: whatever's left isn't
            // a valid `key="val"` pair, so treat it as the (bare) path.
            let path: String = chars[key_start..].iter().collect();
            return Annotations { values, path };
        }
        let key: String = chars[key_start..i].iter().collect();
        i += 1; // consume '='

        if i >= n || chars[i] != '"' {
            // Malformed pair (no opening quote) -- bail out, treating
            // everything from the key onward as the path rather than
            // panicking or silently dropping data.
            let path: String = chars[key_start..].iter().collect();
            return Annotations { values, path };
        }
        i += 1; // consume opening '"'

        let mut value = String::new();
        loop {
            if i >= n {
                // Unterminated value (ran off the end of the string).
                // Keep what we parsed so far and report an empty path.
                values.insert(key, value);
                return Annotations {
                    values,
                    path: String::new(),
                };
            }
            let c = chars[i];
            if c == '\\' && i + 1 < n && chars[i + 1] == '"' {
                value.push('"');
                i += 2;
                continue;
            }
            if c == '"' {
                i += 1; // consume closing '"'
                break;
            }
            value.push(c);
            i += 1;
        }
        values.insert(key, value);

        if i >= n {
            return Annotations {
                values,
                path: String::new(),
            };
        }
        match chars[i] {
            ',' => {
                i += 1;
                continue;
            }
            ':' => {
                i += 1;
                let path: String = chars[i..].iter().collect();
                return Annotations { values, path };
            }
            _ => {
                // Unexpected trailing character; treat the remainder as
                // the path rather than losing it.
                let path: String = chars[i..].iter().collect();
                return Annotations { values, path };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_annotations_bare_path() {
        let a = parse_annotated_uri("media:some/path.mp3");
        assert!(a.is_empty());
        assert_eq!(a.path, "media:some/path.mp3");
    }

    #[test]
    fn single_key() {
        let a = parse_annotated_uri(r#"annotate:title="Song Title":media:some/path.mp3"#);
        assert_eq!(a.len(), 1);
        assert_eq!(a.get("title"), Some("Song Title"));
        assert_eq!(a.path, "media:some/path.mp3");
    }

    #[test]
    fn multiple_keys() {
        let a = parse_annotated_uri(
            r#"annotate:title="Song Title",artist="Some Artist",duration="123.45":media:x.mp3"#,
        );
        assert_eq!(a.len(), 3);
        assert_eq!(a.get("title"), Some("Song Title"));
        assert_eq!(a.get("artist"), Some("Some Artist"));
        assert_eq!(a.get_f64("duration"), Some(123.45));
        assert_eq!(a.path, "media:x.mp3");
    }

    #[test]
    fn embedded_escaped_quote() {
        let a = parse_annotated_uri(r#"annotate:title="She said \"hi\" to me":media:x.mp3"#);
        assert_eq!(a.get("title"), Some(r#"She said "hi" to me"#));
        assert_eq!(a.path, "media:x.mp3");
    }

    #[test]
    fn downstream_keys_of_interest() {
        let uri = concat!(
            r#"annotate:title="T",artist="A",duration="180",song_id="s1","#,
            r#"media_id="m1",sq_id="q1",playlist_id="p1",jingle_mode="true","#,
            r#"azuracast_autocue="true",autocue_cue_in="1.5",autocue_cue_out="170.0","#,
            r#"autocue_fade_in="2.0",autocue_fade_out="3.0",autocue_start_next="165.0","#,
            r#"liq_amplify="3.2 dB",azuracast_cache_key="cachekey123""#,
            ":media:path/to/file.mp3"
        );
        let a = parse_annotated_uri(uri);
        assert_eq!(a.get("title"), Some("T"));
        assert_eq!(a.get("artist"), Some("A"));
        assert_eq!(a.get_f64("duration"), Some(180.0));
        assert_eq!(a.get("song_id"), Some("s1"));
        assert_eq!(a.get("media_id"), Some("m1"));
        assert_eq!(a.get("sq_id"), Some("q1"));
        assert_eq!(a.get("playlist_id"), Some("p1"));
        assert!(a.get_bool("jingle_mode"));
        assert!(a.get_bool("azuracast_autocue"));
        assert_eq!(a.get_f64("autocue_cue_in"), Some(1.5));
        assert_eq!(a.get_f64("autocue_cue_out"), Some(170.0));
        assert_eq!(a.get_f64("autocue_fade_in"), Some(2.0));
        assert_eq!(a.get_f64("autocue_fade_out"), Some(3.0));
        assert_eq!(a.get_f64("autocue_start_next"), Some(165.0));
        assert_eq!(a.get("liq_amplify"), Some("3.2 dB"));
        assert_eq!(a.get("azuracast_cache_key"), Some("cachekey123"));
        assert_eq!(a.path, "media:path/to/file.mp3");
    }

    #[test]
    fn no_prefix_at_all() {
        let a = parse_annotated_uri("/var/azuracast/media/song.mp3");
        assert!(a.is_empty());
        assert_eq!(a.path, "/var/azuracast/media/song.mp3");
    }
}
