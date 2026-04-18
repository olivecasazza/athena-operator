use iii_sdk::III;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;

pub struct StateKV {
    iii: III,
}

impl StateKV {
    pub fn new(iii: III) -> Self {
        Self { iii }
    }

    pub fn get<T: DeserializeOwned>(&self, scope: &str, key: &str) -> Option<T> {
        let result = self
            .iii
            .trigger(json!({
                "function_id": "state::get",
                "payload": { "scope": scope, "key": key }
            }))
            .ok()?;
        serde_json::from_value(result).ok()
    }

    pub fn set<T: Serialize>(&self, scope: &str, key: &str, data: &T) -> Result<(), String> {
        self.iii
            .trigger(json!({
                "function_id": "state::set",
                "payload": { "scope": scope, "key": key, "value": data }
            }))
            .map_err(|e| format!("{:?}", e))?;
        Ok(())
    }
}
