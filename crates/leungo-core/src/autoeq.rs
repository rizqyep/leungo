//! The AutoEq result index: parsing and name search. No networking here.

/// One measurement of one model by one source.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub source: String,
    pub rig: String,
    /// Path under `results/`, still URL-encoded as the index has it.
    pub path: String,
}

/// A model with every usable measurement of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub name: String,
    pub entries: Vec<Entry>,
}

/// Rigs on a 711-type coupler, the one the Harman in-ear target is
/// defined on. 5128 and HMS rigs read differently, so they're left out.
pub fn usable_rig(rig: &str) -> bool {
    matches!(rig, "in-ear" | "711 in-ear" | "GRAS RA0045 in-ear")
}

/// Parses `- [Name](./Source/rig/Name) by Source` lines from `INDEX.md`
/// and groups them by model, keeping usable rigs only.
pub fn parse_index(md: &str) -> Vec<Model> {
    let mut models: Vec<Model> = Vec::new();
    let mut by_name = std::collections::HashMap::new();
    for line in md.lines() {
        let Some(link) = line
            .trim()
            .strip_prefix("- [")
            .and_then(|l| l.split_once("](./"))
            .and_then(|(_, rest)| rest.split_once(") by "))
            .map(|(link, _)| link)
        else {
            continue;
        };
        let mut parts = link.splitn(3, '/');
        let (Some(source), Some(rig), Some(model)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let rig = decode(rig);
        if !usable_rig(&rig) || model.contains('/') {
            continue;
        }
        let name = decode(model);
        let entry = Entry {
            source: decode(source),
            rig,
            path: link.to_string(),
        };
        let i = *by_name.entry(name.clone()).or_insert_with(|| {
            models.push(Model {
                name,
                entries: Vec::new(),
            });
            models.len() - 1
        });
        if !models[i].entries.contains(&entry) {
            models[i].entries.push(entry);
        }
    }
    models
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Models whose name has every query word (as a word prefix, or glued
/// together like "edxpro"), best first: exact names, then fewer extra
/// words, then more sources.
pub fn search<'a>(models: &'a [Model], query: &str, limit: usize) -> Vec<&'a Model> {
    let q = tokens(query);
    if q.is_empty() {
        return Vec::new();
    }
    let q_joined = q.concat();
    let mut hits: Vec<(&Model, (bool, usize, usize))> = models
        .iter()
        .filter_map(|m| {
            let t = tokens(&m.name);
            let joined = t.concat();
            let all = q
                .iter()
                .all(|w| t.iter().any(|n| n.starts_with(w.as_str())))
                || joined.contains(&q_joined);
            all.then(|| {
                let exact = t == q || joined == q_joined;
                (
                    m,
                    (
                        !exact,
                        t.len().saturating_sub(q.len()),
                        usize::MAX - m.entries.len(),
                    ),
                )
            })
        })
        .collect();
    hits.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.name.cmp(&b.0.name)));
    hits.into_iter().take(limit).map(|(m, _)| m).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "# Index\n\n\
- [KZ EDX Pro](./crinacle/711%20in-ear/KZ%20EDX%20Pro) by crinacle on 711\n\
- [KZ EDX Pro](./Super%20Review/in-ear/KZ%20EDX%20Pro) by Super Review\n\
- [KZ EDX Pro](./RikudouGoku/in-ear/KZ%20EDX%20Pro) by RikudouGoku\n\
- [KZ EDX Ultra](./Harpo/in-ear/KZ%20EDX%20Ultra) by Harpo\n\
- [KZ EDX Pro](./Someone/Bruel%20&%20Kjaer%205128%20in-ear/KZ%20EDX%20Pro) by Someone on 5128\n\
- [1MORE Aero (ANC On)](./HypetheSonics/GRAS%20RA0045%20in-ear/1MORE%20Aero%20(ANC%20On)) by HypetheSonics on GRAS RA0045\n\
- [Sennheiser HD 600](./oratory1990/over-ear/Sennheiser%20HD%20600) by oratory1990\n";

    #[test]
    fn groups_sources_and_drops_other_rigs() {
        let m = parse_index(INDEX);
        let names: Vec<&str> = m.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["KZ EDX Pro", "KZ EDX Ultra", "1MORE Aero (ANC On)"]);
        let sources: Vec<&str> = m[0].entries.iter().map(|e| e.source.as_str()).collect();
        assert_eq!(sources, ["crinacle", "Super Review", "RikudouGoku"]);
        assert_eq!(m[0].entries[0].rig, "711 in-ear");
        assert_eq!(
            m[2].entries[0].path,
            "HypetheSonics/GRAS%20RA0045%20in-ear/1MORE%20Aero%20(ANC%20On)"
        );
    }

    #[test]
    fn search_puts_the_exact_name_first() {
        let m = parse_index(INDEX);
        let hits = |q| {
            search(&m, q, 5)
                .iter()
                .map(|m| m.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(hits("kz edx pro"), ["KZ EDX Pro"]);
        assert_eq!(hits("edx"), ["KZ EDX Pro", "KZ EDX Ultra"]);
        assert_eq!(hits("edxpro"), ["KZ EDX Pro"]);
        assert_eq!(hits("aero anc on"), ["1MORE Aero (ANC On)"]);
        assert!(hits("r50i").is_empty());
        assert!(hits("  ").is_empty());
    }
}
