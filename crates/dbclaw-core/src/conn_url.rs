//! Splitting connection URLs into fields (host, port, user, database,
//! options) and building them back, for the field-based connection form.
//!
//! The password is never part of these URLs; it's stored separately.

use crate::model::{DbKind, percent_decode};

/// The editable parts of a connection URL.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UrlParts {
    /// `mongodb+srv://` (DNS seed list; no port).
    pub srv: bool,
    /// Host name, `[ipv6]`, or a comma-separated host list (`a:1,b:2`) for
    /// Mongo replica sets, in which case `port` is `None`.
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub database: String,
    /// Query string without the leading `?`, e.g. `sslmode=require`.
    pub options: String,
}

pub fn default_port(kind: DbKind) -> u16 {
    match kind {
        DbKind::Postgres => 5432,
        DbKind::Mongo => 27017,
    }
}

/// Parse a URL (without password) into its parts.
pub fn parse(url: &str) -> Result<(DbKind, UrlParts), String> {
    let url = url.trim();
    let kind = DbKind::from_url(url)
        .ok_or_else(|| "the URL must start with postgres://, postgresql://, mongodb:// or mongodb+srv://".to_string())?;
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let mut parts = UrlParts { srv: scheme.eq_ignore_ascii_case("mongodb+srv"), ..Default::default() };

    let rest = rest.split('#').next().unwrap_or(rest);
    let (before_query, query) = rest.split_once('?').unwrap_or((rest, ""));
    parts.options = query.to_string();
    let (authority, path) = before_query.split_once('/').unwrap_or((before_query, ""));
    parts.database = percent_decode(path);

    let hostport = match authority.rsplit_once('@') {
        Some((userinfo, hostport)) => {
            let user = userinfo.split_once(':').map_or(userinfo, |(u, _)| u);
            parts.user = percent_decode(user);
            hostport
        }
        None => authority,
    };

    if hostport.contains(',') || parts.srv {
        parts.host = hostport.to_string();
        if parts.srv && hostport.contains(':') {
            return Err("mongodb+srv:// URLs can't have a port".into());
        }
    } else if let Some(v6) = hostport.strip_prefix('[') {
        let (addr, after) = v6.split_once(']').ok_or("unclosed [ in IPv6 host")?;
        parts.host = format!("[{addr}]");
        if let Some(port) = after.strip_prefix(':') {
            parts.port = Some(port.parse().map_err(|_| format!("invalid port `{port}`"))?);
        }
    } else if let Some((host, port)) = hostport.rsplit_once(':') {
        parts.host = host.to_string();
        parts.port = Some(port.parse().map_err(|_| format!("invalid port `{port}`"))?);
    } else {
        parts.host = hostport.to_string();
    }
    Ok((kind, parts))
}

/// Build a URL from parts. An empty host means `localhost`.
pub fn build(kind: DbKind, parts: &UrlParts) -> String {
    let mut url = String::from(match kind {
        DbKind::Postgres => "postgres://",
        DbKind::Mongo if parts.srv => "mongodb+srv://",
        DbKind::Mongo => "mongodb://",
    });
    let user = parts.user.trim();
    if !user.is_empty() {
        url.push_str(&percent_encode(user));
        url.push('@');
    }
    let host = parts.host.trim();
    url.push_str(if host.is_empty() { "localhost" } else { host });
    let multi_host = host.contains(',');
    if let Some(port) = parts.port.filter(|_| !parts.srv && !multi_host) {
        url.push_str(&format!(":{port}"));
    }
    let database = parts.database.trim();
    let options = parts.options.trim().trim_start_matches('?');
    if !database.is_empty() || !options.is_empty() {
        url.push('/');
        url.push_str(&percent_encode(database));
    }
    if !options.is_empty() {
        url.push('?');
        url.push_str(options);
    }
    url
}

/// Percent-encode everything except RFC 3986 unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(host: &str, port: Option<u16>, user: &str, db: &str, opts: &str) -> UrlParts {
        UrlParts {
            srv: false,
            host: host.into(),
            port,
            user: user.into(),
            database: db.into(),
            options: opts.into(),
        }
    }

    #[test]
    fn parse_postgres() {
        let (k, p) = parse("postgres://bob@db.example.com:5433/shop?sslmode=require").unwrap();
        assert_eq!(k, DbKind::Postgres);
        assert_eq!(p, parts("db.example.com", Some(5433), "bob", "shop", "sslmode=require"));
        let (_, p) = parse("postgresql://localhost").unwrap();
        assert_eq!(p, parts("localhost", None, "", "", ""));
        let (_, p) = parse("postgres://u%40corp@[::1]:5432/my%20db").unwrap();
        assert_eq!(p, parts("[::1]", Some(5432), "u@corp", "my db", ""));
        // A password in the URL is ignored (it's split out separately).
        let (_, p) = parse("postgres://bob:secret@h/db").unwrap();
        assert_eq!(p.user, "bob");
        assert!(parse("postgres://h:notaport/db").is_err());
        assert!(parse("mysql://h").is_err());
    }

    #[test]
    fn parse_mongo() {
        let (k, p) = parse("mongodb://admin@a:27017,b:27018/app?replicaSet=rs0&authSource=admin").unwrap();
        assert_eq!(k, DbKind::Mongo);
        assert_eq!(p, parts("a:27017,b:27018", None, "admin", "app", "replicaSet=rs0&authSource=admin"));
        let (_, p) = parse("mongodb+srv://u@cluster0.x.mongodb.net/app?retryWrites=true").unwrap();
        assert!(p.srv);
        assert_eq!(p.host, "cluster0.x.mongodb.net");
        assert_eq!(p.port, None);
        let (_, p) = parse("mongodb://localhost:27019/?authSource=admin").unwrap();
        assert_eq!(p, parts("localhost", Some(27019), "", "", "authSource=admin"));
    }

    #[test]
    fn build_urls() {
        assert_eq!(
            build(DbKind::Postgres, &parts("db", Some(5432), "bob", "shop", "sslmode=require")),
            "postgres://bob@db:5432/shop?sslmode=require"
        );
        assert_eq!(build(DbKind::Postgres, &parts("", None, "", "", "")), "postgres://localhost");
        assert_eq!(
            build(DbKind::Postgres, &parts("h", None, "a b@c", "my db", "")),
            "postgres://a%20b%40c@h/my%20db"
        );
        assert_eq!(
            build(DbKind::Mongo, &parts("h", Some(27017), "", "", "?authSource=admin")),
            "mongodb://h:27017/?authSource=admin"
        );
        let srv = UrlParts { srv: true, port: Some(1), ..parts("c.mongodb.net", None, "u", "app", "") };
        assert_eq!(build(DbKind::Mongo, &srv), "mongodb+srv://u@c.mongodb.net/app");
        assert_eq!(build(DbKind::Mongo, &parts("a:1,b:2", Some(9), "", "", "")), "mongodb://a:1,b:2");
    }

    #[test]
    fn round_trip() {
        for url in [
            "postgres://bob@db.example.com:5433/shop?sslmode=require",
            "postgres://u%40corp@[::1]:5432/my%20db",
            "mongodb://admin@a:27017,b:27018/app?replicaSet=rs0",
            "mongodb+srv://u@cluster0.x.mongodb.net/app?retryWrites=true",
            "mongodb://localhost:27019/?authSource=admin",
        ] {
            let (k, p) = parse(url).unwrap();
            assert_eq!(build(k, &p), url, "{url}");
        }
    }
}
