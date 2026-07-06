use crate::api::schema::ErrorBody;
use crate::remote_target::RemoteRoutePlanError;

pub(super) fn rewrite_remote_response_id_value(
    response: &str,
    id: &str,
) -> std::io::Result<serde_json::Value> {
    let mut value: serde_json::Value = serde_json::from_str(response).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid remote API response JSON: {err}"),
        )
    })?;
    let Some(object) = value.as_object_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "remote API response must be a JSON object",
        ));
    };
    if !object.contains_key("result") && !object.contains_key("error") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "remote API response must contain result or error",
        ));
    }
    object.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    Ok(value)
}

pub(super) fn rewrite_remote_response_id(response: &str, id: &str) -> std::io::Result<String> {
    let value = rewrite_remote_response_id_value(response, id)?;
    serde_json::to_string(&value).map_err(std::io::Error::other)
}

pub(super) fn remote_route_plan_error_body(err: RemoteRoutePlanError) -> ErrorBody {
    match err {
        RemoteRoutePlanError::Parse(err) => ErrorBody {
            code: "remote_target_error".to_string(),
            message: err.to_string(),
        },
        RemoteRoutePlanError::UnknownHost(host) => ErrorBody {
            code: "remote_target_error".to_string(),
            message: format!("unknown remote host: {host}"),
        },
    }
}
