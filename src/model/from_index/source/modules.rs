//! The `mod` declarations a file writes, whatever their `cfg`. rust-analyzer
//! reads only the modules its configuration turns on; a reader sees them all.

use super::Source;
use super::tokens::Kind;

/// A module whose text is in a file of its own: `mod name;`.
#[derive(Debug, PartialEq, Eq)]
pub struct Declared {
    /// The inline modules around it, outermost first, each by the directory
    /// its modules' files are in: its `#[path]`, or else its name.
    pub within: Vec<String>,
    pub name: String,
    /// Its `#[path = ".."]`.
    pub path: Option<String>,
}

impl Source {
    /// Every `mod name;` in the file and its inline modules. One in a
    /// function, a macro or any other block can only come from a macro's
    /// expansion, which this does not follow.
    pub fn module_declarations(&self) -> Vec<Declared> {
        let tokens = &self.tokens;
        let mut declared = Vec::new();
        // For each open bracket: the inline module it opens, by its
        // directory; `None` for any other bracket.
        let mut open: Vec<Option<String>> = Vec::new();
        let mut path = None;
        let mut at = 0;
        while at < tokens.len() {
            let token = tokens[at];
            if self.is(token, "#") {
                let inner = self.is_at(at + 1, "!");
                let bracket = at + 1 + usize::from(inner);
                if self.is_at(bracket, "[")
                    && let Some(close) = self.matching(bracket)
                {
                    if !inner {
                        path = self.path_attribute(bracket + 1, close).or(path);
                    }
                    at = close + 1;
                    continue;
                }
            }
            if self.is(token, "pub") {
                at = match self.is_at(at + 1, "(") {
                    true => self.matching(at + 1).unwrap_or(at) + 1,
                    false => at + 1,
                };
                continue;
            }
            let name = tokens.get(at + 1).filter(|next| next.kind == Kind::Ident);
            if let (true, Some(&name)) = (self.is(token, "mod"), name) {
                let name = self.text(name);
                let name = name.strip_prefix("r#").unwrap_or(name).to_owned();
                if self.is_at(at + 2, "{") {
                    open.push(Some(path.take().unwrap_or(name)));
                    at += 3;
                    continue;
                }
                if self.is_at(at + 2, ";") && open.iter().all(Option::is_some) {
                    declared.push(Declared {
                        within: open.iter().flatten().cloned().collect(),
                        name,
                        path: path.take(),
                    });
                }
            }
            match token.kind {
                Kind::Open => open.push(None),
                Kind::Close => {
                    open.pop();
                }
                _ => {}
            }
            path = None;
            at += 1;
        }
        declared
    }

    fn is_at(&self, at: usize, text: &str) -> bool {
        self.tokens
            .get(at)
            .is_some_and(|token| self.is(*token, text))
    }

    /// The path an attribute's tokens, from `start` to `close`, give when it
    /// is `path = ".."`.
    fn path_attribute(&self, start: usize, close: usize) -> Option<String> {
        let &[name, equals, value] = &self.tokens[start..close] else {
            return None;
        };
        let plain = self.is(name, "path") && self.is(equals, "=") && value.kind == Kind::Literal;
        plain.then(|| string(self.text(value))).flatten()
    }
}

/// A string literal's text: `"a/b.rs"`, `r"a\b.rs"`, `r#"a.rs"#`.
fn string(literal: &str) -> Option<String> {
    if let Some(raw) = literal.strip_prefix('r') {
        let quoted = raw.trim_matches('#');
        return Some(quoted.strip_prefix('"')?.strip_suffix('"')?.to_owned());
    }
    let quoted = literal.strip_prefix('"')?.strip_suffix('"')?;
    Some(quoted.replace("\\\\", "\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(text: &str) -> Vec<Declared> {
        Source::new(text.to_owned()).module_declarations()
    }

    fn module(within: &[&str], name: &str, path: Option<&str>) -> Declared {
        Declared {
            within: within
                .iter()
                .map(|&directory| directory.to_owned())
                .collect(),
            name: name.to_owned(),
            path: path.map(str::to_owned),
        }
    }

    #[test]
    fn every_declaration_counts_whatever_its_cfg_or_visibility() {
        let text = "\
            mod a;\n\
            #[cfg(all(test, my_tests))]\n\
            pub(crate) mod b;\n\
            #[cfg(feature = \"x\")] #[allow(dead_code)] pub mod r#type;\n\
            /// Docs.\n\
            pub(in crate::a) mod c;\n\
            mod inline_with_items { fn f() {} }\n\
            use a::b;\n";
        let names = ["a", "b", "type", "c"].map(|name| module(&[], name, None));
        assert_eq!(declared(text), names);
    }

    #[test]
    fn a_path_attribute_belongs_to_the_module_it_comes_before() {
        let text = "\
            #![allow(unused)]\n\
            #[path = \"generated/out.rs\"]\n\
            #[cfg(test)]\n\
            mod out;\n\
            mod after;\n\
            #[path = r\"raw.rs\"] mod raw;\n\
            #[doc = \"not a path\"] mod documented;\n";
        assert_eq!(
            declared(text),
            [
                module(&[], "out", Some("generated/out.rs")),
                module(&[], "after", None),
                module(&[], "raw", Some("raw.rs")),
                module(&[], "documented", None),
            ]
        );
    }

    #[test]
    fn a_declaration_in_an_inline_module_is_within_its_directory() {
        let text = "\
            mod outer {\n\
                #[path = \"elsewhere\"]\n\
                pub mod inner { mod deep; }\n\
                mod beside;\n\
            }\n\
            mod top;\n";
        assert_eq!(
            declared(text),
            [
                module(&["outer", "elsewhere"], "deep", None),
                module(&["outer"], "beside", None),
                module(&[], "top", None),
            ]
        );
    }

    #[test]
    fn only_a_macros_expansion_declares_in_a_function_a_macro_or_an_impl() {
        let text = "\
            fn f() { mod in_function; }\n\
            macro_rules! declare { ($name:ident) => { mod $name; mod fixed; }; }\n\
            impl T { }\n\
            // mod in_a_comment;\n\
            const TEXT: &str = \"mod in_a_string;\";\n\
            mod kept;\n";
        assert_eq!(declared(text), [module(&[], "kept", None)]);
    }
}
