//! `race.cfg`: the race list (`[races] RACE: name`, in index order) and, per race, its enemies.

use crate::cbin::{self, Value};
use crate::source::AssetSource;
use anyhow::Result;

#[derive(Debug, Clone, Default)]
pub struct Races {
    /// Lower-case race names; the index is the race id.
    pub names: Vec<String>,
    /// `enemies[a]` lists the race ids race `a` is hostile to.
    pub enemies: Vec<Vec<usize>>,
}

impl Races {
    pub fn load(src: &AssetSource) -> Result<Races> {
        Self::parse(&src.read("race.cfg")?)
    }

    pub fn parse(raw: &[u8]) -> Result<Races> {
        let groups = cbin::parse(raw)?;
        let names: Vec<String> = groups
            .iter()
            .filter(|g| g.name.eq_ignore_ascii_case("races"))
            .flat_map(|g| g.get_all("RACE"))
            .filter_map(|it| it.values.first().and_then(Value::as_str).map(str::to_ascii_lowercase))
            .collect();
        let mut enemies = vec![vec![]; names.len()];
        for (i, n) in names.iter().enumerate() {
            if let Some(g) = groups.iter().find(|g| g.name.eq_ignore_ascii_case(n)) {
                enemies[i] = g
                    .get_all("ENEMY")
                    .filter_map(|it| it.values.first().and_then(Value::as_str))
                    .filter_map(|e| names.iter().position(|x| x.eq_ignore_ascii_case(e)))
                    .collect();
            }
        }
        Ok(Races { names, enemies })
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(name))
    }

    pub fn hostile(&self, a: usize, b: usize) -> bool {
        self.enemies.get(a).is_some_and(|e| e.contains(&b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn races() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../../pff")) else { return };
        let r = Races::load(&src).unwrap();
        assert_eq!(r.names.len(), 19);
        assert_eq!(r.names[1], "bora");
        assert!(r.hostile(r.index("agt").unwrap(), r.index("cmi").unwrap()));
    }
}
