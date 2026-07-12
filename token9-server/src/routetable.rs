use std::collections::HashMap;

use crate::store::RouteSet;
use crate::store::sqlite::SqliteStore;

/// In-memory routing cache: logical model -> its ordered target set.
#[derive(Debug, Default, Clone)]
pub struct RouteTable {
    map: HashMap<String, RouteSet>,
}

impl RouteTable {
    pub async fn load(store: &SqliteStore) -> anyhow::Result<Self> {
        let sets = store.load_routes().await?;
        let mut map = HashMap::with_capacity(sets.len());
        for s in sets {
            map.insert(s.model_id.clone(), s);
        }
        Ok(RouteTable { map })
    }

    pub fn resolve(&self, model_id: &str) -> Option<RouteSet> {
        self.map.get(model_id).cloned()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
