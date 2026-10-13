//! JSON-RPC 2.0 envelopes, shared by the LSP client ([`crate::lsp`]), the
//! builtin Steel language server, and the MCP server ([`crate::mcp`]): the
//! constructors for every outgoing message and [`Message`], the one place an
//! incoming message is classified.
//!
//! Ids are kept as raw [`Value`]s: the spec allows a number or a string (and
//! servers in the wild send `null`), and a reply must echo the id exactly as
//! received.

use serde_json::{Value, json};

/// An incoming JSON-RPC message, classified the way both of our peers need:
/// by whether it has a string `method` and whether it has an `id` key (an
/// `"id": null` counts as present).
#[derive(Debug, PartialEq)]
pub enum Message {
    /// A `method` and an `id`: the peer expects a reply.
    Request {
        id: Value,
        method: String,
        /// `null` when absent.
        params: Value,
    },
    /// A `method` and no `id`.
    Notification {
        method: String,
        /// `null` when absent.
        params: Value,
    },
    /// An `id` and no (string) `method`: a reply to one of our requests.
    Response {
        id: Value,
        /// `null` when absent (an error reply, or a genuine `null` result).
        result: Value,
        error: Option<Value>,
    },
    /// Neither an `id` nor a string `method`.
    Invalid,
}

impl From<Value> for Message {
    fn from(mut message: Value) -> Message {
        let Some(object) = message.as_object_mut() else {
            return Message::Invalid;
        };
        let id = object.remove("id");
        let mut take = |key: &str| object.remove(key).unwrap_or(Value::Null);
        match (take("method"), id) {
            (Value::String(method), Some(id)) => Message::Request {
                id,
                method,
                params: take("params"),
            },
            (Value::String(method), None) => Message::Notification {
                method,
                params: take("params"),
            },
            (_, Some(id)) => Message::Response {
                id,
                result: take("result"),
                error: object.remove("error"),
            },
            (_, None) => Message::Invalid,
        }
    }
}

/// A request with numeric `id`.
pub fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// A notification (no `id`, no reply expected).
pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// A successful reply to request `id`.
pub fn response(id: impl Into<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.into(), "result": result })
}

/// An error reply to request `id`.
pub fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_method_and_id() {
        assert_eq!(
            Message::from(json!({"jsonrpc": "2.0", "id": "a", "method": "m", "params": [1]})),
            Message::Request {
                id: json!("a"),
                method: "m".into(),
                params: json!([1])
            }
        );
        // An explicit `null` id still makes it a request.
        assert!(matches!(
            Message::from(json!({"id": null, "method": "m"})),
            Message::Request {
                id: Value::Null,
                params: Value::Null,
                ..
            }
        ));
        assert_eq!(
            Message::from(json!({"method": "n"})),
            Message::Notification {
                method: "n".into(),
                params: Value::Null
            }
        );
        // A null result is still a result; an error reply has a null one.
        assert_eq!(
            Message::from(json!({"id": 3, "result": null})),
            Message::Response {
                id: json!(3),
                result: Value::Null,
                error: None
            }
        );
        assert_eq!(
            Message::from(json!({"id": 3, "error": {"code": 1}})),
            Message::Response {
                id: json!(3),
                result: Value::Null,
                error: Some(json!({"code": 1}))
            }
        );
        // A non-string method doesn't make a request.
        assert!(matches!(
            Message::from(json!({"id": 1, "method": 5})),
            Message::Response { .. }
        ));
        assert_eq!(Message::from(json!({})), Message::Invalid);
        assert_eq!(Message::from(json!([1])), Message::Invalid);
    }

    #[test]
    fn envelopes_serialize_in_wire_order() {
        assert_eq!(
            response(1, json!({})).to_string(),
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#
        );
        assert_eq!(
            error_response(json!("x"), -32601, "nope").to_string(),
            r#"{"jsonrpc":"2.0","id":"x","error":{"code":-32601,"message":"nope"}}"#
        );
        assert_eq!(
            request(2, "m", Value::Null).to_string(),
            r#"{"jsonrpc":"2.0","id":2,"method":"m","params":null}"#
        );
    }
}
