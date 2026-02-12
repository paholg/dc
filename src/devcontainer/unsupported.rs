//! We don't support all of devcontainer features, and we want to make that
//! clear when we load devcontainer.json. These helpers are for that.

use serde::{Deserialize, Deserializer, de::IgnoredAny};

pub(crate) trait Unsupported {
    const FIELD: &'static str;

    fn warn<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        tracing::error!("`{}` is not supported; ignoring", Self::FIELD);
        let val = T::deserialize(deserializer)?;
        Ok(val)
    }

    fn error<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
    {
        IgnoredAny::deserialize(deserializer)?;
        Err(serde::de::Error::custom(format!(
            "`{}` is not supported at this time",
            Self::FIELD
        )))
    }
}

macro_rules! unsupported {
    ($name:ident, $($rest:ident),+ $(,)?) => {
        unsupported!($name);
        unsupported!($($rest),+);
    };
    ($name:ident) => {
        #[allow(non_camel_case_types)]
        pub(crate) struct $name;
        impl $crate::devcontainer::unsupported::Unsupported for $name {
            const FIELD: &'static str = stringify!($name);
        }
    };
}

unsupported!(
    Image,
    Dockerfile,
    features,
    overrideFeatureInstallOrder,
    secrets,
    otherPortsAttributes,
    mounts
);
