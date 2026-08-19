use std::collections::HashMap;

use strum::{EnumDiscriminants, IntoStaticStr};

use crate::container::ContainerStatus;

/// A single filter clause for list/event endpoints.
///
/// Repeating a category (e.g. two `Label`s) is OR within the category;
/// different categories are AND across categories — the same semantics as
/// docker's underlying filter JSON.
#[derive(Debug, Clone, EnumDiscriminants)]
#[strum_discriminants(name(FilterCategory))]
#[strum_discriminants(derive(IntoStaticStr))]
#[strum_discriminants(strum(serialize_all = "lowercase"))]
pub enum Filter {
    /// `key=VALUE` match if `value: Some`; key-existence match if `value: None`.
    Label {
        key: String,
        value: Option<String>,
    },
    Status(ContainerStatus),
    Id(String),
    Name(String),
}

impl Filter {
    /// The name this clause demands, if it is a [`Filter::Name`].
    pub(crate) fn as_name(&self) -> Option<&str> {
        match self {
            Self::Name(name) => Some(name),
            _ => None,
        }
    }

    fn value(&self) -> String {
        match self {
            Self::Label {
                key,
                value: Some(v),
            } => format!("{key}={v}"),
            Self::Label { key, value: None } => key.clone(),
            Self::Status(status) => status.to_string(),
            Self::Id(id) => id.clone(),
            Self::Name(name) => name.clone(),
        }
    }
}

/// Extension trait so callers can write `filters.to_query_json()`.
pub(crate) trait FilterSliceExt {
    /// Render as the JSON object that docker expects in the `filters` query
    /// parameter (e.g. `{"label":["k=v"],"status":["running"]}`).
    fn to_docker_query(&self) -> String;
}

impl FilterSliceExt for [Filter] {
    fn to_docker_query(&self) -> String {
        let mut by_category: HashMap<&'static str, Vec<String>> = HashMap::new();
        for f in self {
            let category: FilterCategory = f.into();
            by_category
                .entry(category.into())
                .or_default()
                .push(f.value());
        }
        serde_json::to_string(&by_category).expect("string-keyed map always serializes")
    }
}
