use std::fmt;

use serde::de::{self, Unexpected};
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardPort {
    pub service: Option<String>,
    pub port: u16,
}

impl fmt::Display for ForwardPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(service) = &self.service {
            f.write_str(service)?;
            f.write_str(":")?;
            self.port.fmt(f)
        } else {
            self.port.fmt(f)
        }
    }
}

impl Serialize for ForwardPort {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.service.is_some() {
            serializer.collect_str(&self)
        } else {
            serializer.serialize_u16(self.port)
        }
    }
}

impl<'de> Deserialize<'de> for ForwardPort {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(u16),
            String(String),
        }

        match Raw::deserialize(deserializer)? {
            Raw::Number(port) => Ok(ForwardPort {
                service: None,
                port,
            }),
            Raw::String(s) => {
                if let Some((service, port)) = s.split_once(':') {
                    let port = port.parse::<u16>().map_err(|_| {
                        de::Error::invalid_value(Unexpected::Str(&s), &"a valid port mapping")
                    })?;
                    Ok(ForwardPort {
                        service: Some(service.to_string()),
                        port,
                    })
                } else {
                    Err(de::Error::invalid_value(
                        Unexpected::Str(&s),
                        &"a port number or \"service:port\" mapping",
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_number() {
        let pm: ForwardPort = serde_json::from_str("3000").unwrap();
        assert_eq!(
            pm,
            ForwardPort {
                service: None,
                port: 3000
            }
        );
    }

    #[test]
    fn from_string_mapping() {
        let pm: ForwardPort = serde_json::from_str("\"redis:3001\"").unwrap();
        assert_eq!(
            pm,
            ForwardPort {
                service: Some("redis".into()),
                port: 3001
            }
        );
    }

    #[test]
    fn serialize_mapping() {
        let pm = ForwardPort {
            service: Some("foo".into()),
            port: 3001,
        };
        assert_eq!(serde_json::to_string(&pm).unwrap(), "\"foo:3001\"");
    }

    #[test]
    fn serialize_plain_mapping() {
        let pm = ForwardPort {
            service: None,
            port: 3001,
        };
        assert_eq!(serde_json::to_string(&pm).unwrap(), "3001");
    }

    #[test]
    fn invalid_string() {
        assert!(serde_json::from_str::<ForwardPort>("\"abc\"").is_err());
    }

    #[test]
    fn invalid_mapping() {
        assert!(serde_json::from_str::<ForwardPort>("\"abc:def\"").is_err());
        assert!(serde_json::from_str::<ForwardPort>("\"3000:abc\"").is_err());
    }
}
