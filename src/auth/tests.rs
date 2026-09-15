use super::*;
use serde_json::json;

fn chatgpt_auth() -> Value {
    let claims = json!({"email": "synthetic@example.test"});
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    json!({"tokens": {
        "account_id": "synthetic-workspace",
        "id_token": format!("synthetic.{payload}.sig"),
        "access_token": "synthetic-access",
        "refresh_token": "synthetic-refresh"
    }})
}

#[test]
fn legacy_chatgpt_documents_allow_absent_or_null_material() {
    for mode in [None, Some(Value::Null)] {
        for null_material in [false, true] {
            let mut document = chatgpt_auth();
            if let Some(mode) = &mode {
                document["auth_mode"] = mode.clone();
            }
            if null_material {
                for field in [
                    "OPENAI_API_KEY",
                    "personal_access_token",
                    "bedrock_api_key",
                    "bedrock_access_keys",
                ] {
                    document[field] = Value::Null;
                }
            }
            assert!(identity_value(&document).is_ok());
        }
    }
}

#[test]
fn inferred_non_chatgpt_modes_reject_complete_chatgpt_tokens() {
    for (field, material) in [
        ("OPENAI_API_KEY", json!("sk-synthetic")),
        ("OPENAI_API_KEY", json!("")),
        ("personal_access_token", json!("at-synthetic")),
        ("personal_access_token", json!("")),
        ("bedrock_api_key", json!({"api_key": "", "region": ""})),
        (
            "bedrock_api_key",
            json!(["synthetic-api", "synthetic-region"]),
        ),
        (
            "bedrock_access_keys",
            json!({"access_key_id": "", "secret_access_key": ""}),
        ),
        (
            "bedrock_access_keys",
            json!(["synthetic-id", "synthetic-secret"]),
        ),
        (
            "bedrock_access_keys",
            json!(["synthetic-id", "synthetic-secret", null]),
        ),
    ] {
        for mode in [None, Some(Value::Null)] {
            let mut document = chatgpt_auth();
            if let Some(mode) = mode {
                document["auth_mode"] = mode;
            }
            document[field] = material.clone();
            let error = identity_value(&document).err().unwrap();
            assert!(error.to_string().contains("another authentication mode"));
        }
    }
}

#[test]
fn explicit_chatgpt_takes_precedence_without_rewriting_credentials() {
    let home = tempfile::tempdir().unwrap();
    for (api_key, access_keys) in [
        (
            json!({"api_key": "", "region": ""}),
            json!({"access_key_id": "", "secret_access_key": "", "session_token": null}),
        ),
        (
            json!(["synthetic-api", "synthetic-region"]),
            json!(["synthetic-id", "synthetic-secret"]),
        ),
        (
            json!(["synthetic-api", "synthetic-region"]),
            json!(["synthetic-id", "synthetic-secret", null]),
        ),
        (
            json!(["synthetic-api", "synthetic-region"]),
            json!(["synthetic-id", "synthetic-secret", "synthetic-session"]),
        ),
    ] {
        let document = json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": "sk-synthetic",
            "personal_access_token": "",
            "bedrock_api_key": api_key,
            "bedrock_access_keys": access_keys,
            "tokens": chatgpt_auth()["tokens"]
        });
        fsutil_write(home.path(), &document);
        let original = std::fs::read(home.path().join("auth.json")).unwrap();
        let (saved, identity) = credentials(home.path()).unwrap();
        assert_eq!(saved, document);
        assert!(verify(home.path(), &Some(identity)).is_ok());
        assert_eq!(
            std::fs::read(home.path().join("auth.json")).unwrap(),
            original
        );
    }
}

#[test]
fn malformed_modes_and_auth_material_fail_all_shared_parser_entrypoints() {
    let home = tempfile::tempdir().unwrap();
    let expected = Some(identity_value(&chatgpt_auth()).unwrap());
    let mut rejected = Vec::new();
    for mode in [
        json!("apikey"),
        json!("personalAccessToken"),
        json!("bedrockApiKey"),
        json!("bedrockAccessKeys"),
        json!("chatgptAuthTokens"),
        json!("headers"),
        json!("agentIdentity"),
        json!("unknown"),
        json!(""),
        json!(false),
        json!(7),
        json!([]),
        json!({}),
    ] {
        let mut document = chatgpt_auth();
        document["auth_mode"] = mode;
        rejected.push(document);
    }
    for (field, malformed) in [
        ("OPENAI_API_KEY", json!(false)),
        ("OPENAI_API_KEY", json!(7)),
        ("OPENAI_API_KEY", json!([])),
        ("OPENAI_API_KEY", json!({})),
        ("personal_access_token", json!(false)),
        ("bedrock_api_key", json!("synthetic")),
        ("bedrock_api_key", json!({"api_key": "synthetic"})),
        ("bedrock_api_key", json!({"api_key": 7, "region": ""})),
        ("bedrock_api_key", json!([])),
        ("bedrock_api_key", json!(["synthetic-api"])),
        ("bedrock_api_key", json!(["synthetic-api", 7])),
        (
            "bedrock_api_key",
            json!(["synthetic-api", "synthetic-region", "extra"]),
        ),
        ("bedrock_access_keys", json!([])),
        ("bedrock_access_keys", json!(["synthetic-id"])),
        ("bedrock_access_keys", json!(["synthetic-id", null])),
        (
            "bedrock_access_keys",
            json!(["synthetic-id", "synthetic-secret", false]),
        ),
        (
            "bedrock_access_keys",
            json!(["synthetic-id", "synthetic-secret", null, "extra"]),
        ),
        (
            "bedrock_access_keys",
            json!({"access_key_id": "", "secret_access_key": null}),
        ),
        (
            "bedrock_access_keys",
            json!({"access_key_id": "", "secret_access_key": "", "session_token": false}),
        ),
    ] {
        let mut document = chatgpt_auth();
        document["auth_mode"] = json!("chatgpt");
        document[field] = malformed;
        rejected.push(document);
    }
    let mut hybrid = chatgpt_auth();
    hybrid["OPENAI_API_KEY"] = json!("sk-synthetic");
    rejected.push(hybrid);
    for document in rejected {
        fsutil_write(home.path(), &document);
        let original = std::fs::read(home.path().join("auth.json")).unwrap();
        assert!(identity(home.path()).is_err());
        assert!(require(home.path()).is_err());
        assert!(credentials(home.path()).is_err());
        assert!(verify(home.path(), &expected).is_err());
        assert_eq!(
            std::fs::read(home.path().join("auth.json")).unwrap(),
            original
        );
    }
}

fn fsutil_write(home: &Path, document: &Value) {
    crate::fsutil::atomic_json(&home.join("auth.json"), document).unwrap();
}
