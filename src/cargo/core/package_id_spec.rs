use std::collections::HashMap;
use std::fmt;

use semver::Version;
use serde::{de, ser};
use url::Url;

use crate::core::interning::InternedString;
use crate::core::PackageId;
use crate::util::errors::{CargoResult, CargoResultExt};
use crate::util::{validate_package_name, IntoUrl, ToSemver};

/// Some or all of the data required to identify a package:
///
///  1. the package name (a `String`, required)
///  2. the package version (a `Version`, optional)
///  3. the package source (a `Url`, optional)
///
/// If any of the optional fields are omitted, then the package ID may be ambiguous, there may be
/// more than one package/version/url combo that will match. However, often just the name is
/// sufficient to uniquely define a package ID.
#[derive(Clone, PartialEq, Eq, Debug, Hash, Ord, PartialOrd)]
pub struct PackageIdSpec {
    name: InternedString,
    version: Option<Version>,
    url: Option<Url>,
}

impl PackageIdSpec {
    /// Parses a spec string and returns a `PackageIdSpec` if the string was valid.
    ///
    /// # Examples
    /// Some examples of valid strings
    ///
    /// ```
    /// use cargo::core::PackageIdSpec;
    ///
    /// let specs = vec![
    ///     "https://crates.io/foo#1.2.3",
    ///     "https://crates.io/foo#bar:1.2.3",
    ///     "crates.io/foo",
    ///     "crates.io/foo#1.2.3",
    ///     "crates.io/foo#bar",
    ///     "crates.io/foo#bar:1.2.3",
    ///     "foo",
    ///     "foo:1.2.3",
    /// ];
    /// for spec in specs {
    ///     assert!(PackageIdSpec::parse(spec).is_ok());
    /// }
    pub fn parse(spec: &str) -> CargoResult<PackageIdSpec> {
        if spec.contains('/') {
            if let Ok(url) = spec.into_url() {
                return PackageIdSpec::from_url(url);
            }
            if !spec.contains("://") {
                if let Ok(url) = Url::parse(&format!("cargo://{}", spec)) {
                    return PackageIdSpec::from_url(url);
                }
            }
        }
        let mut parts = spec.splitn(2, ':');
        let name = parts.next().unwrap();
        let version = match parts.next() {
            Some(version) => Some(version.to_semver()?),
            None => None,
        };
        validate_package_name(name, "pkgid", "")?;
        Ok(PackageIdSpec {
            name: InternedString::new(name),
            version,
            url: None,
        })
    }

    /// Roughly equivalent to `PackageIdSpec::parse(spec)?.query(i)`
    pub fn query_str<I>(spec: &str, i: I) -> CargoResult<PackageId>
    where
        I: IntoIterator<Item = PackageId>,
    {
        let spec = PackageIdSpec::parse(spec)
            .chain_err(|| anyhow::format_err!("invalid package ID specification: `{}`", spec))?;
        spec.query(i)
    }

    /// Convert a `PackageId` to a `PackageIdSpec`, which will have both the `Version` and `Url`
    /// fields filled in.
    pub fn from_package_id(package_id: PackageId) -> PackageIdSpec {
        PackageIdSpec {
            name: package_id.name(),
            version: Some(package_id.version().clone()),
            url: Some(package_id.source_id().url().clone()),
        }
    }

    /// Tries to convert a valid `Url` to a `PackageIdSpec`.
    fn from_url(mut url: Url) -> CargoResult<PackageIdSpec> {
        if url.query().is_some() {
            anyhow::bail!("cannot have a query string in a pkgid: {}", url)
        }
        let frag = url.fragment().map(|s| s.to_owned());
        url.set_fragment(None);
        let (name, version) = {
            let mut path = url
                .path_segments()
                .ok_or_else(|| anyhow::format_err!("pkgid urls must have a path: {}", url))?;
            let path_name = path.next_back().ok_or_else(|| {
                anyhow::format_err!(
                    "pkgid urls must have at least one path \
                     component: {}",
                    url
                )
            })?;
            match frag {
                Some(fragment) => {
                    let mut parts = fragment.splitn(2, ':');
                    let name_or_version = parts.next().unwrap();
                    match parts.next() {
                        Some(part) => {
                            let version = part.to_semver()?;
                            (InternedString::new(name_or_version), Some(version))
                        }
                        None => {
                            if name_or_version.chars().next().unwrap().is_alphabetic() {
                                (InternedString::new(name_or_version), None)
                            } else {
                                let version = name_or_version.to_semver()?;
                                (InternedString::new(path_name), Some(version))
                            }
                        }
                    }
                }
                None => (InternedString::new(path_name), None),
            }
        };
        Ok(PackageIdSpec {
            name,
            version,
            url: Some(url),
        })
    }

    pub fn name(&self) -> InternedString {
        self.name
    }

    pub fn version(&self) -> Option<&Version> {
        self.version.as_ref()
    }

    pub fn url(&self) -> Option<&Url> {
        self.url.as_ref()
    }

    pub fn set_url(&mut self, url: Url) {
        self.url = Some(url);
    }

    /// Checks whether the given `PackageId` matches the `PackageIdSpec`.
    pub fn matches(&self, package_id: PackageId) -> bool {
        if self.name() != package_id.name() {
            return false;
        }

        if let Some(ref v) = self.version {
            if v != package_id.version() {
                return false;
            }
        }

        match self.url {
            Some(ref u) => u == package_id.source_id().url(),
            None => true,
        }
    }

    /// Checks a list of `PackageId`s to find 1 that matches this `PackageIdSpec`. If 0, 2, or
    /// more are found, then this returns an error.
    pub fn query<I>(&self, i: I) -> CargoResult<PackageId>
    where
        I: IntoIterator<Item = PackageId>,
    {
        let mut ids = i.into_iter().filter(|p| self.matches(*p));
        let ret = match ids.next() {
            Some(id) => id,
            None => anyhow::bail!(
                "package ID specification `{}` \
                 matched no packages",
                self
            ),
        };
        return match ids.next() {
            Some(other) => {
                let mut msg = format!(
                    "There are multiple `{}` packages in \
                     your project, and the specification \
                     `{}` is ambiguous.\n\
                     Please re-run this command \
                     with `-p <spec>` where `<spec>` is one \
                     of the following:",
                    self.name(),
                    self
                );
                let mut vec = vec![ret, other];
                vec.extend(ids);
                minimize(&mut msg, &vec, self);
                Err(anyhow::format_err!("{}", msg))
            }
            None => Ok(ret),
        };

        fn minimize(msg: &mut String, ids: &[PackageId], spec: &PackageIdSpec) {
            let mut version_cnt = HashMap::new();
            for id in ids {
                *version_cnt.entry(id.version()).or_insert(0) += 1;
            }
            for id in ids {
                if version_cnt[id.version()] == 1 {
                    msg.push_str(&format!("\n  {}:{}", spec.name(), id.version()));
                } else {
                    msg.push_str(&format!("\n  {}", PackageIdSpec::from_package_id(*id)));
                }
            }
        }
    }
}

impl fmt::Display for PackageIdSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut printed_name = false;
        match self.url {
            Some(ref url) => {
                if url.scheme() == "cargo" {
                    write!(f, "{}{}", url.host().unwrap(), url.path())?;
                } else {
                    write!(f, "{}", url)?;
                }
                if url.path_segments().unwrap().next_back().unwrap() != &*self.name {
                    printed_name = true;
                    write!(f, "#{}", self.name)?;
                }
            }
            None => {
                printed_name = true;
                write!(f, "{}", self.name)?
            }
        }
        if let Some(ref v) = self.version {
            write!(f, "{}{}", if printed_name { ":" } else { "#" }, v)?;
        }
        Ok(())
    }
}

impl ser::Serialize for PackageIdSpec {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: ser::Serializer,
    {
        self.to_string().serialize(s)
    }
}

impl<'de> de::Deserialize<'de> for PackageIdSpec {
    fn deserialize<D>(d: D) -> Result<PackageIdSpec, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        let string = String::deserialize(d)?;
        PackageIdSpec::parse(&string).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::PackageIdSpec;
    use crate::core::interning::InternedString;
    use crate::core::{PackageId, SourceId};
    use crate::util::ToSemver;
    use url::Url;

    #[test]
    fn good_parsing() {
        fn ok(spec: &str, expected: PackageIdSpec) {
            let parsed = PackageIdSpec::parse(spec).unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(parsed.to_string(), spec);
        }

        ok(
            "https://crates.io/foo#1.2.3",
            PackageIdSpec {
                name: InternedString::new("foo"),
                version: Some("1.2.3".to_semver().unwrap()),
                url: Some(Url::parse("https://crates.io/foo").unwrap()),
            },
        );
        ok(
            "https://crates.io/foo#bar:1.2.3",
            PackageIdSpec {
                name: InternedString::new("bar"),
                version: Some("1.2.3".to_semver().unwrap()),
                url: Some(Url::parse("https://crates.io/foo").unwrap()),
            },
        );
        ok(
            "crates.io/foo",
            PackageIdSpec {
                name: InternedString::new("foo"),
                version: None,
                url: Some(Url::parse("cargo://crates.io/foo").unwrap()),
            },
        );
        ok(
            "crates.io/foo#1.2.3",
            PackageIdSpec {
                name: InternedString::new("foo"),
                version: Some("1.2.3".to_semver().unwrap()),
                url: Some(Url::parse("cargo://crates.io/foo").unwrap()),
            },
        );
        ok(
            "crates.io/foo#bar",
            PackageIdSpec {
                name: InternedString::new("bar"),
                version: None,
                url: Some(Url::parse("cargo://crates.io/foo").unwrap()),
            },
        );
        ok(
            "crates.io/foo#bar:1.2.3",
            PackageIdSpec {
                name: InternedString::new("bar"),
                version: Some("1.2.3".to_semver().unwrap()),
                url: Some(Url::parse("cargo://crates.io/foo").unwrap()),
            },
        );
        ok(
            "foo",
            PackageIdSpec {
                name: InternedString::new("foo"),
                version: None,
                url: None,
            },
        );
        ok(
            "foo:1.2.3",
            PackageIdSpec {
                name: InternedString::new("foo"),
                version: Some("1.2.3".to_semver().unwrap()),
                url: None,
            },
        );
    }

    #[test]
    fn bad_parsing() {
        assert!(PackageIdSpec::parse("baz:").is_err());
        assert!(PackageIdSpec::parse("baz:*").is_err());
        assert!(PackageIdSpec::parse("baz:1.0").is_err());
        assert!(PackageIdSpec::parse("https://baz:1.0").is_err());
        assert!(PackageIdSpec::parse("https://#baz:1.0").is_err());
    }

    #[test]
    fn matching() {
        let url = Url::parse("https://example.com").unwrap();
        let sid = SourceId::for_registry(&url).unwrap();
        let foo = PackageId::new("foo", "1.2.3", sid).unwrap();
        let bar = PackageId::new("bar", "1.2.3", sid).unwrap();

        assert!(PackageIdSpec::parse("foo").unwrap().matches(foo));
        assert!(!PackageIdSpec::parse("foo").unwrap().matches(bar));
        assert!(PackageIdSpec::parse("foo:1.2.3").unwrap().matches(foo));
        assert!(!PackageIdSpec::parse("foo:1.2.2").unwrap().matches(foo));
    }
}
