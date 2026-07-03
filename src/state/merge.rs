use serde_json::Value;

/// Merge a delta patch into the current state, following the F1 live timing
/// conventions: objects merge recursively, and arrays are patched by objects
/// whose keys are numeric indices (`{"3": {...}}` updates/appends element 3).
/// Anything else replaces the target wholesale.
pub fn merge(target: &mut Value, patch: Value) {
    match patch {
        Value::Object(patch_map) => match target {
            Value::Object(target_map) => {
                for (k, v) in patch_map {
                    match target_map.get_mut(&k) {
                        Some(slot) => merge(slot, v),
                        None => {
                            target_map.insert(k, v);
                        }
                    }
                }
            }
            Value::Array(arr) => {
                for (k, v) in patch_map {
                    if let Ok(idx) = k.parse::<usize>() {
                        if idx < arr.len() {
                            merge(&mut arr[idx], v);
                        } else {
                            while arr.len() < idx {
                                arr.push(Value::Null);
                            }
                            arr.push(v);
                        }
                    }
                }
            }
            other => *other = Value::Object(patch_map),
        },
        other => *target = other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merges_nested_objects() {
        let mut state = json!({"Lines": {"1": {"Position": "1", "InPit": false}}});
        merge(&mut state, json!({"Lines": {"1": {"InPit": true}}}));
        assert_eq!(state["Lines"]["1"]["Position"], "1");
        assert_eq!(state["Lines"]["1"]["InPit"], true);
    }

    #[test]
    fn patches_array_by_index_key() {
        let mut state = json!({"Messages": [{"Message": "a"}]});
        merge(&mut state, json!({"Messages": {"1": {"Message": "b"}}}));
        assert_eq!(state["Messages"].as_array().unwrap().len(), 2);
        assert_eq!(state["Messages"][1]["Message"], "b");
    }

    #[test]
    fn scalar_replaces() {
        let mut state = json!({"Status": "1"});
        merge(&mut state, json!({"Status": "4"}));
        assert_eq!(state["Status"], "4");
    }

    #[test]
    fn array_index_gap_pads_with_null() {
        let mut state = json!([{"a": 1}]);
        merge(&mut state, json!({"3": {"a": 4}}));
        assert_eq!(state.as_array().unwrap().len(), 4);
        assert_eq!(state[3]["a"], 4);
    }
}
