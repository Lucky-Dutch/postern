use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DEFAULT_STATEMENT_TIMEOUT_SECS: u32 = 30;

fn default_statement_timeout() -> u32 {
    DEFAULT_STATEMENT_TIMEOUT_SECS
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: Uuid,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub ssl: SslMode,
    pub color: usize,
    /// Folder shown in the saved server list. Empty means ungrouped.
    #[serde(default)]
    pub group: String,
    /// Path to a PEM file with additional trusted CA certificates.
    #[serde(default)]
    pub ssl_root_cert: String,
    /// Paths to a PEM client certificate and its PKCS#8 PEM private key.
    #[serde(default)]
    pub ssl_client_cert: String,
    #[serde(default)]
    pub ssl_client_key: String,
    /// Starts every session read-only with `default_transaction_read_only`.
    #[serde(default)]
    pub read_only: bool,
    /// SQL executed after each session connects, such as `SET search_path`.
    #[serde(default)]
    pub startup_sql: String,
    /// Extra libpq `options`, such as `-c search_path=app` or `endpoint=...`.
    #[serde(default)]
    pub connection_options: String,
    /// Statement timeout for the SQL editor session. Zero disables it.
    #[serde(default = "default_statement_timeout")]
    pub statement_timeout_secs: u32,
    #[serde(default)]
    pub ssh: Option<SshTunnelConfig>,
}

impl Default for ConnectionProfile {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: "Local PostgreSQL".into(),
            host: "localhost".into(),
            port: 5432,
            database: "postgres".into(),
            username: std::env::var("USER").unwrap_or_else(|_| "postgres".into()),
            ssl: SslMode::Prefer,
            color: 0,
            group: String::new(),
            ssl_root_cert: String::new(),
            ssl_client_cert: String::new(),
            ssl_client_key: String::new(),
            read_only: false,
            startup_sql: String::new(),
            connection_options: String::new(),
            statement_timeout_secs: DEFAULT_STATEMENT_TIMEOUT_SECS,
            ssh: None,
        }
    }
}

impl ConnectionProfile {
    pub fn validate(&self) -> anyhow::Result<()> {
        for (label, value) in [
            ("Name", &self.name),
            ("Host", &self.host),
            ("Database", &self.database),
            ("User", &self.username),
        ] {
            anyhow::ensure!(!value.trim().is_empty(), "{label} is required.");
            anyhow::ensure!(!value.contains('\0'), "{label} contains a NUL character.");
        }
        anyhow::ensure!(self.port > 0, "Port must be between 1 and 65535.");
        anyhow::ensure!(
            self.ssl_client_cert.trim().is_empty() == self.ssl_client_key.trim().is_empty(),
            "A client certificate needs both a certificate file and a key file."
        );
        if let Some(ssh) = &self.ssh {
            ssh.validate()?;
        }
        Ok(())
    }

    /// Parses a `postgresql://` URL or a libpq `key=value` connection string and
    /// returns the profile with the password it contained. SSL verification modes,
    /// certificate paths, and `options` are mapped to profile settings.
    pub fn from_connection_string(text: &str) -> anyhow::Result<(Self, String)> {
        use std::str::FromStr;
        let text = text.trim();
        let url = text.starts_with("postgres://") || text.starts_with("postgresql://");
        let (rest, params) = if url {
            split_url_params(text)
        } else {
            split_keyword_params(text)?
        };
        let mut profile = Self::default();
        let mut kept = Vec::new();
        for (key, value) in params {
            match key.as_str() {
                "sslmode" => {
                    profile.ssl = match value.as_str() {
                        "disable" => SslMode::Disable,
                        "allow" | "prefer" => SslMode::Prefer,
                        "require" => SslMode::Require,
                        "verify-ca" => SslMode::VerifyCa,
                        "verify-full" => SslMode::VerifyFull,
                        other => anyhow::bail!("Unsupported sslmode {other:?}."),
                    }
                }
                "sslrootcert" => profile.ssl_root_cert = value,
                "sslcert" => profile.ssl_client_cert = value,
                "sslkey" => profile.ssl_client_key = value,
                "options" => profile.connection_options = value,
                _ => kept.push((key, value)),
            }
        }
        let rebuilt = if url {
            let query = kept
                .iter()
                .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            if query.is_empty() {
                rest
            } else {
                format!("{rest}?{query}")
            }
        } else {
            let mut parts = vec![rest];
            parts.extend(
                kept.iter()
                    .map(|(k, v)| format!("{k}={}", quote_keyword_value(v))),
            );
            parts.join(" ")
        };
        let config = tokio_postgres::Config::from_str(&rebuilt)
            .map_err(|error| anyhow::anyhow!("Invalid PostgreSQL connection string: {error}"))?;
        anyhow::ensure!(
            config.get_hosts().len() <= 1,
            "Multiple hosts are not supported in connection profiles yet."
        );
        profile.host = match config.get_hosts().first() {
            Some(tokio_postgres::config::Host::Tcp(host)) => host.clone(),
            #[cfg(unix)]
            Some(tokio_postgres::config::Host::Unix(path)) => path.to_string_lossy().into(),
            None => "localhost".into(),
        };
        profile.port = config.get_ports().first().copied().unwrap_or(5432);
        profile.database = config.get_dbname().unwrap_or("postgres").to_owned();
        profile.name = profile.database.clone();
        if let Some(user) = config.get_user() {
            profile.username = user.to_owned();
        }
        let password = String::from_utf8(config.get_password().unwrap_or_default().to_vec())?;
        profile.validate()?;
        Ok((profile, password))
    }

    /// The settings that decide which server and login a connection reaches.
    /// Database lists discovered for one endpoint are invalid for another.
    pub fn same_endpoint(&self, other: &Self) -> bool {
        self.id == other.id
            && self.host == other.host
            && self.port == other.port
            && self.username == other.username
            && self.ssl == other.ssl
            && self.ssl_root_cert == other.ssl_root_cert
            && self.ssl_client_cert == other.ssl_client_cert
            && self.ssl_client_key == other.ssl_client_key
            && self.ssh == other.ssh
    }
}

fn split_url_params(url: &str) -> (String, Vec<(String, String)>) {
    let (rest, query) = url.split_once('?').unwrap_or((url, ""));
    let params = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect();
    (rest.to_owned(), params)
}

/// Splits a libpq keyword string. Every pair is returned; the rest is empty.
fn split_keyword_params(text: &str) -> anyhow::Result<(String, Vec<(String, String)>)> {
    let mut params = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            break;
        }
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| *c != '=' && !c.is_whitespace()) {
            key.push(c);
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        anyhow::ensure!(
            chars.next() == Some('='),
            "Expected key=value in the connection string."
        );
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut value = String::new();
        if chars.next_if_eq(&'\'').is_some() {
            loop {
                match chars.next() {
                    Some('\\') => value.extend(chars.next()),
                    Some('\'') => break,
                    Some(c) => value.push(c),
                    None => anyhow::bail!("Unterminated quote in the connection string."),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                if c == '\\' {
                    value.extend(chars.next());
                } else {
                    value.push(c);
                }
            }
        }
        params.push((key, value));
    }
    Ok((String::new(), params))
}

fn quote_keyword_value(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = text.get(index + 1..index + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// An SSH tunnel run by the system `ssh` client. Authentication uses the SSH agent
/// or an identity file; passwords are never passed to `ssh`. Host keys are checked
/// against `~/.ssh/known_hosts` by OpenSSH.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshTunnelConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub identity_file: String,
    /// Adds unknown host keys on first use (`StrictHostKeyChecking=accept-new`).
    /// Changed keys are always rejected.
    #[serde(default)]
    pub accept_new_host_key: bool,
}

impl Default for SshTunnelConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            user: std::env::var("USER").unwrap_or_default(),
            identity_file: String::new(),
            accept_new_host_key: false,
        }
    }
}

impl SshTunnelConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.host.trim().is_empty(), "SSH host is required.");
        anyhow::ensure!(self.port > 0, "SSH port must be between 1 and 65535.");
        for (label, value) in [
            ("SSH host", &self.host),
            ("SSH user", &self.user),
            ("SSH identity file", &self.identity_file),
        ] {
            anyhow::ensure!(
                !value.trim_start().starts_with('-'),
                "{label} cannot start with '-'."
            );
            anyhow::ensure!(
                !value.contains(['\0', '\n', '\r']),
                "{label} contains a control character."
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SslMode {
    Disable,
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

impl SslMode {
    pub const ALL: [Self; 5] = [
        Self::Disable,
        Self::Prefer,
        Self::Require,
        Self::VerifyCa,
        Self::VerifyFull,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Disable => "Disable",
            Self::Prefer => "Prefer",
            Self::Require => "Require",
            Self::VerifyCa => "Verify CA",
            Self::VerifyFull => "Verify full",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Relation {
    pub schema: String,
    pub name: String,
    pub kind: String,
    pub estimated_rows: i64,
    /// Schema and name of the partitioned table this relation is a partition of.
    pub parent: Option<(String, String)>,
}

impl Relation {
    pub fn kind_label(&self) -> &'static str {
        match self.kind.as_str() {
            "r" => "table",
            "p" => "partitioned table",
            "v" => "view",
            "m" => "materialized view",
            "f" => "foreign table",
            _ => "relation",
        }
    }

    /// The keyword used in DDL for this relation, such as `MATERIALIZED VIEW`.
    pub fn ddl_keyword(&self) -> &'static str {
        match self.kind.as_str() {
            "v" => "VIEW",
            "m" => "MATERIALIZED VIEW",
            "f" => "FOREIGN TABLE",
            _ => "TABLE",
        }
    }

    pub fn qualified_name(&self) -> String {
        format!(
            "{}.{}",
            crate::sql::quote_ident(&self.schema),
            crate::sql::quote_ident(&self.name)
        )
    }
    pub fn is_table(&self) -> bool {
        matches!(self.kind.as_str(), "r" | "p")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Column {
    pub name: String,
    /// The declared type including modifiers, such as `character varying(20)`.
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary_key: bool,
    pub generated: bool,
    pub comment: Option<String>,
    /// Schema-qualified type without modifiers, used for parameter casts. Casting
    /// to `varchar(n)` would silently truncate; assignment to the column instead
    /// enforces the modifier and reports over-long values.
    pub cast_type: String,
    /// False for types without a default btree ordering, such as json or point.
    pub sortable: bool,
}

impl Column {
    /// The type used to cast text parameters for this column.
    pub fn cast(&self) -> &str {
        if self.cast_type.is_empty() {
            &self.data_type
        } else {
            &self.cast_type
        }
    }
}

#[derive(Clone, Debug)]
pub struct MutationTarget {
    pub relation: Relation,
    pub columns: Vec<Column>,
}

/// A foreign key from `schema.table (columns)` to
/// `referenced_schema.referenced_table (referenced_columns)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub referenced_schema: String,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

/// A user-defined function or procedure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Routine {
    pub oid: u32,
    pub schema: String,
    pub name: String,
    /// Identity arguments, as accepted by DROP FUNCTION.
    pub arguments: String,
    pub result: String,
    pub procedure: bool,
}

impl Routine {
    pub fn signature(&self) -> String {
        format!(
            "{}.{}({})",
            crate::sql::quote_ident(&self.schema),
            crate::sql::quote_ident(&self.name),
            self.arguments
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub definition: String,
    /// The index implements a primary key, unique, or exclusion constraint.
    pub constraint: bool,
    pub bytes: Option<i64>,
    pub scans: Option<i64>,
}

/// Size and activity statistics of a table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableStats {
    pub total_bytes: i64,
    pub table_bytes: i64,
    pub index_bytes: i64,
    pub live_rows: Option<i64>,
    pub dead_rows: Option<i64>,
    pub sequential_scans: Option<i64>,
    pub index_scans: Option<i64>,
    pub last_vacuum: Option<String>,
    pub last_analyze: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TableStructure {
    pub columns: Vec<Column>,
    pub constraints: Vec<(String, String)>,
    pub indexes: Vec<IndexInfo>,
    pub stats: Option<TableStats>,
    /// Generated DDL that recreates the relation.
    pub definition: String,
    pub comment: Option<String>,
    /// Foreign keys declared on this table.
    pub foreign_keys: Vec<ForeignKey>,
    /// Foreign keys in other tables that reference this table.
    pub referenced_by: Vec<ForeignKey>,
    /// Granted privileges as (grantee, comma-separated privileges).
    pub privileges: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub affected: u64,
    pub truncated: bool,
    /// First keyword of the statement that produced this result, such as SELECT.
    pub command: String,
    /// The statement that produced this result, used to export it in full.
    pub sql: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransactionState {
    #[default]
    Unknown,
    Idle,
    Open,
    Failed,
}

#[derive(Clone, Debug, Default)]
pub struct QueryOutput {
    pub results: Vec<QueryResult>,
    pub elapsed_ms: u128,
    pub error: Option<String>,
    /// Character offset of the error in the executed SQL, when PostgreSQL reports one.
    pub error_offset: Option<usize>,
    /// NOTICE, WARNING, and similar messages raised while executing.
    pub messages: Vec<String>,
    /// Number of statements in the executed SQL and how many completed.
    pub statements: usize,
    pub completed: usize,
    pub transaction: TransactionState,
}

#[derive(Clone, Debug, Default)]
pub struct TablePage {
    pub result: QueryResult,
    pub has_more: bool,
    pub elapsed_ms: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FilterOperator {
    #[default]
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Matches,
    IsNull,
    IsNotNull,
    InList,
    NotInList,
}

impl FilterOperator {
    pub const ALL: [Self; 15] = [
        Self::Contains,
        Self::NotContains,
        Self::StartsWith,
        Self::EndsWith,
        Self::Equal,
        Self::NotEqual,
        Self::Less,
        Self::LessOrEqual,
        Self::Greater,
        Self::GreaterOrEqual,
        Self::Matches,
        Self::IsNull,
        Self::IsNotNull,
        Self::InList,
        Self::NotInList,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::NotContains => "does not contain",
            Self::StartsWith => "begins with",
            Self::EndsWith => "ends with",
            Self::Equal => "equals",
            Self::NotEqual => "does not equal",
            Self::Less => "<",
            Self::LessOrEqual => "≤",
            Self::Greater => ">",
            Self::GreaterOrEqual => "≥",
            Self::Matches => "matches regex",
            Self::IsNull => "is NULL",
            Self::IsNotNull => "is not NULL",
            Self::InList => "is in list",
            Self::NotInList => "is not in list",
        }
    }
    pub fn needs_value(self) -> bool {
        !matches!(self, Self::IsNull | Self::IsNotNull)
    }
    pub fn is_list(self) -> bool {
        matches!(self, Self::InList | Self::NotInList)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub column: String,
    pub operator: FilterOperator,
    pub value: String,
}

/// Row filter for the table browser: conditions combined with AND (or OR when
/// `any` is set), plus an optional SQL expression written by the user.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterSet {
    pub conditions: Vec<Filter>,
    pub any: bool,
    pub custom_sql: String,
}

impl FilterSet {
    pub fn single(filter: Filter) -> Self {
        Self {
            conditions: vec![filter],
            ..Default::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty() && self.custom_sql.trim().is_empty()
    }
}

/// Where NULL values sort. `Default` follows PostgreSQL: last when ascending,
/// first when descending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NullsOrder {
    #[default]
    Default,
    First,
    Last,
}

impl NullsOrder {
    pub const ALL: [Self; 3] = [Self::Default, Self::First, Self::Last];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "NULLs: default",
            Self::First => "NULLs first",
            Self::Last => "NULLs last",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortColumn {
    pub name: String,
    pub descending: bool,
}

#[derive(Clone, Debug, Default)]
pub struct BrowseRequest {
    pub relation: Relation,
    pub columns: Vec<Column>,
    pub filter: FilterSet,
    pub sort: Vec<SortColumn>,
    /// Where NULL values sort in every sort column.
    pub nulls: NullsOrder,
    pub page: usize,
    pub page_size: usize,
}

impl BrowseRequest {
    /// Keeps the filter, sort, and page of a refreshed table where they still
    /// apply to its current columns. A changed column list returns to page one.
    pub fn adapted_to(mut self, columns: &[Column]) -> Self {
        let exists = |name: &str| columns.iter().any(|c| c.name == name);
        if self.columns != columns {
            self.page = 0;
        }
        self.filter.conditions.retain(|c| exists(&c.column));
        self.sort.retain(|s| exists(&s.name));
        self.columns = columns.to_vec();
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum InsertValue {
    #[default]
    Default,
    Null,
    Text(String),
}

/// Light or dark appearance. `System` follows macOS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System appearance",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

/// Display settings remembered between launches.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UiState {
    #[serde(default)]
    pub appearance: Appearance,
    /// Hidden column names by table key (`profile id/database/schema.table`).
    #[serde(default)]
    pub hidden_columns: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
}

/// One row of values as text. `None` is SQL NULL.
pub type Row = Vec<Option<String>>;

/// Changed `(column index, new value)` pairs of one row.
pub type CellChanges = Vec<(usize, Option<String>)>;

/// Staged grid changes for one table, saved together in one transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowChanges {
    /// Each update pairs the complete original row with its changed cells.
    pub updates: Vec<(Row, CellChanges)>,
    pub inserts: Vec<Vec<InsertValue>>,
    pub deletes: Vec<Vec<Option<String>>>,
}

impl RowChanges {
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.inserts.is_empty() && self.deletes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.updates.len() + self.inserts.len() + self.deletes.len()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryDocument {
    pub id: Uuid,
    pub title: String,
    pub sql: String,
    pub path: Option<std::path::PathBuf>,
    /// Folder shown in the query list. Empty means ungrouped.
    #[serde(default)]
    pub folder: String,
}

impl Default for QueryDocument {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            title: "SQL Query".into(),
            sql: "SELECT current_database(), version();".into(),
            path: None,
            folder: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_urls_with_verification_modes_certificates_and_options() {
        let (profile, password) = ConnectionProfile::from_connection_string(
            " postgresql://app%40x:s%3Acret@db.example.com:6543/sales?sslmode=verify-full&sslrootcert=%2Fetc%2Fca.pem&options=endpoint%3Dep-1&connect_timeout=5 ",
        )
        .unwrap();
        assert_eq!(profile.host, "db.example.com");
        assert_eq!(profile.port, 6543);
        assert_eq!(profile.database, "sales");
        assert_eq!(profile.username, "app@x");
        assert_eq!(password, "s:cret");
        assert_eq!(profile.ssl, SslMode::VerifyFull);
        assert_eq!(profile.ssl_root_cert, "/etc/ca.pem");
        assert_eq!(profile.connection_options, "endpoint=ep-1");
        let (profile, password) = ConnectionProfile::from_connection_string(
            "host=/tmp port=5433 dbname='my db' user=me password='it\\'s' sslmode=verify-ca sslcert=/c.pem sslkey=/k.pem",
        )
        .unwrap();
        assert_eq!(profile.host, "/tmp");
        assert_eq!(profile.database, "my db");
        assert_eq!(password, "it's");
        assert_eq!(profile.ssl, SslMode::VerifyCa);
        assert_eq!(
            (
                profile.ssl_client_cert.as_str(),
                profile.ssl_client_key.as_str()
            ),
            ("/c.pem", "/k.pem")
        );
        assert!(
            ConnectionProfile::from_connection_string("postgresql://h/db?sslmode=bogus").is_err()
        );
        assert!(
            ConnectionProfile::from_connection_string("postgresql://h/db?sslcert=/only-cert.pem")
                .is_err()
        );
        assert!(ConnectionProfile::from_connection_string("host='open").is_err());
    }

    #[test]
    fn old_profiles_load_with_new_defaults() {
        let json = r#"[{"id":"7f1d3f7e-4a43-4e9c-9b0a-3f5f2c9d8e11","name":"Old","host":"h","port":5432,"database":"d","username":"u","ssl":"Prefer","color":2}]"#;
        let profiles: Vec<ConnectionProfile> = serde_json::from_str(json).unwrap();
        assert_eq!(
            profiles[0].statement_timeout_secs,
            DEFAULT_STATEMENT_TIMEOUT_SECS
        );
        assert!(profiles[0].ssh.is_none() && !profiles[0].read_only);
    }

    #[test]
    fn ssh_settings_reject_option_injection() {
        let mut ssh = SshTunnelConfig {
            host: "-oProxyCommand=evil".into(),
            ..Default::default()
        };
        assert!(ssh.validate().is_err());
        ssh.host = "bastion".into();
        ssh.identity_file = "-i/tmp/x".into();
        assert!(ssh.validate().is_err());
        ssh.identity_file = "~/.ssh/id_ed25519".into();
        assert!(ssh.validate().is_ok());
    }

    #[test]
    fn refreshed_requests_keep_valid_filters_and_sorts() {
        let column = |name: &str| Column {
            name: name.into(),
            ..Default::default()
        };
        let request = BrowseRequest {
            columns: vec![column("a"), column("b")],
            filter: FilterSet {
                conditions: vec![
                    Filter {
                        column: "a".into(),
                        ..Default::default()
                    },
                    Filter {
                        column: "b".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            sort: vec![SortColumn {
                name: "b".into(),
                descending: true,
            }],
            page: 3,
            ..Default::default()
        };
        let same = request.clone().adapted_to(&[column("a"), column("b")]);
        assert_eq!(
            (same.page, same.filter.conditions.len(), same.sort.len()),
            (3, 2, 1)
        );
        let changed = request.adapted_to(&[column("a")]);
        assert_eq!(
            (
                changed.page,
                changed.filter.conditions.len(),
                changed.sort.len()
            ),
            (0, 1, 0)
        );
    }
}

/// An action on a table, view, or other relation from the object list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelationAction {
    Truncate {
        cascade: bool,
        restart_identity: bool,
    },
    Drop {
        cascade: bool,
    },
    Rename(String),
    VacuumAnalyze,
    Analyze,
    Reindex,
    RefreshMaterializedView,
}

/// A column in the structure editor. `original` is the column's current name, or
/// `None` for a new column.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnDraft {
    pub original: Option<String>,
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    /// Default expression; empty means no default.
    pub default: String,
    pub comment: String,
    /// Part of the primary key when creating a table.
    pub primary_key: bool,
    /// Generated or identity columns keep their expression; defaults do not apply.
    pub generated: bool,
    pub dropped: bool,
}

/// A constraint in the structure editor, such as `CHECK (price > 0)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConstraintDraft {
    pub original: Option<String>,
    pub name: String,
    pub definition: String,
    pub dropped: bool,
}

/// An index in the structure editor. New indexes use `columns` (the parenthesized
/// key list), `unique`, and `method`; existing indexes keep their definition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexDraft {
    pub original: Option<String>,
    pub name: String,
    pub definition: String,
    pub columns: String,
    pub unique: bool,
    pub method: String,
    pub dropped: bool,
}

/// The editable structure of a table, compared with its original to produce DDL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableDraft {
    pub schema: String,
    pub name: String,
    pub comment: String,
    pub columns: Vec<ColumnDraft>,
    pub constraints: Vec<ConstraintDraft>,
    pub indexes: Vec<IndexDraft>,
}

impl TableDraft {
    pub fn from_structure(relation: &Relation, structure: &TableStructure) -> Self {
        Self {
            schema: relation.schema.clone(),
            name: relation.name.clone(),
            comment: structure.comment.clone().unwrap_or_default(),
            columns: structure
                .columns
                .iter()
                .map(|c| ColumnDraft {
                    original: Some(c.name.clone()),
                    name: c.name.clone(),
                    data_type: c.data_type.clone(),
                    nullable: c.nullable,
                    default: if c.generated {
                        String::new()
                    } else {
                        c.default.clone().unwrap_or_default()
                    },
                    comment: c.comment.clone().unwrap_or_default(),
                    primary_key: c.primary_key,
                    generated: c.generated,
                    dropped: false,
                })
                .collect(),
            constraints: structure
                .constraints
                .iter()
                .map(|(name, definition)| ConstraintDraft {
                    original: Some(name.clone()),
                    name: name.clone(),
                    definition: definition.clone(),
                    dropped: false,
                })
                .collect(),
            indexes: structure
                .indexes
                .iter()
                .filter(|index| !index.constraint)
                .map(|index| IndexDraft {
                    original: Some(index.name.clone()),
                    name: index.name.clone(),
                    definition: index.definition.clone(),
                    ..Default::default()
                })
                .collect(),
        }
    }
}

/// One server session from `pg_stat_activity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionActivity {
    pub pid: i32,
    pub user: String,
    pub database: String,
    pub application: String,
    pub client: String,
    pub state: String,
    pub wait: String,
    /// Seconds since the current query, or the transaction, started.
    pub query_seconds: Option<f64>,
    pub transaction_seconds: Option<f64>,
    pub query: String,
    /// The session belongs to this Postern connection.
    pub own: bool,
}

/// A database role and its attributes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleInfo {
    pub name: String,
    pub superuser: bool,
    pub login: bool,
    pub create_database: bool,
    pub create_role: bool,
    pub replication: bool,
    pub bypass_rls: bool,
    pub connection_limit: i32,
    pub valid_until: Option<String>,
    pub member_of: Vec<String>,
}

/// A kind of index or table finding from the index advisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AdviceKind {
    /// Left behind by a failed `CREATE INDEX CONCURRENTLY`; never used, still maintained.
    InvalidIndex,
    /// A foreign key whose columns do not lead any index.
    ForeignKeyWithoutIndex,
    /// The same columns as another index.
    DuplicateIndex,
    /// The leading columns of a longer index.
    RedundantIndex,
    /// No index scans since statistics were last reset.
    UnusedIndex,
    /// Large tables read mostly by sequential scans.
    SequentialScans,
    /// Many dead rows waiting for VACUUM.
    DeadRows,
}

impl AdviceKind {
    pub const ALL: [Self; 7] = [
        Self::InvalidIndex,
        Self::ForeignKeyWithoutIndex,
        Self::DuplicateIndex,
        Self::RedundantIndex,
        Self::UnusedIndex,
        Self::SequentialScans,
        Self::DeadRows,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::InvalidIndex => "Invalid indexes",
            Self::ForeignKeyWithoutIndex => "Foreign keys without an index",
            Self::DuplicateIndex => "Duplicate indexes",
            Self::RedundantIndex => "Redundant indexes",
            Self::UnusedIndex => "Unused indexes",
            Self::SequentialScans => "Tables read by sequential scans",
            Self::DeadRows => "Tables with many dead rows",
        }
    }

    pub fn explanation(self) -> &'static str {
        match self {
            Self::InvalidIndex => {
                "A failed CREATE INDEX CONCURRENTLY left these indexes. Queries never use them, but every write still updates them."
            }
            Self::ForeignKeyWithoutIndex => {
                "Deleting or updating a referenced row scans the whole referencing table, and joins on these columns cannot use an index."
            }
            Self::DuplicateIndex => {
                "These indexes have exactly the same columns as another index. Each one slows writes and uses space."
            }
            Self::RedundantIndex => {
                "These indexes are the leading columns of a longer index, which can answer the same queries."
            }
            Self::UnusedIndex => {
                "No query has used these indexes since statistics were reset. Statistics are per server, so check replicas before dropping. Indexes that start with a foreign key column are not listed, because deleting referenced rows needs them."
            }
            Self::SequentialScans => {
                "These large tables are read mostly by sequential scans. Find the queries in Slow queries and EXPLAIN them."
            }
            Self::DeadRows => {
                "Dead rows make scans slower and the table larger until VACUUM removes them."
            }
        }
    }
}

/// One finding of the index advisor, with SQL that fixes it when there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexAdvice {
    pub kind: AdviceKind,
    pub schema: String,
    pub table: String,
    /// The index or constraint concerned, or empty for table findings.
    pub object: String,
    pub detail: String,
    /// Space the fix would free, for index findings.
    pub bytes: Option<i64>,
    pub fix: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexReport {
    pub items: Vec<IndexAdvice>,
    /// When usage statistics were last reset, if known.
    pub stats_since: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SlowQueryOrder {
    #[default]
    TotalTime,
    MeanTime,
    Calls,
    Rows,
}

impl SlowQueryOrder {
    pub const ALL: [Self; 4] = [Self::TotalTime, Self::MeanTime, Self::Calls, Self::Rows];

    pub fn label(self) -> &'static str {
        match self {
            Self::TotalTime => "Total time",
            Self::MeanTime => "Mean time",
            Self::Calls => "Calls",
            Self::Rows => "Rows",
        }
    }
}

/// What the slow query list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlowQueryRequest {
    pub order: SlowQueryOrder,
    /// Statements of every database on the server, not only the current one.
    pub all_databases: bool,
    /// Statements that read `pg_catalog` or `information_schema`, such as the
    /// catalog queries of database clients, including this one.
    pub include_catalog: bool,
}

/// Aggregated statistics of one normalized statement from `pg_stat_statements`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlowQuery {
    pub query: String,
    pub calls: i64,
    pub total_ms: f64,
    pub mean_ms: f64,
    pub max_ms: f64,
    pub rows: i64,
    /// Share of the total execution time of the listed statements, in percent.
    pub share: f64,
    /// Fraction of shared buffer reads served from cache.
    pub hit_ratio: Option<f64>,
    pub user: String,
    pub database: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum StatementsStatus {
    #[default]
    Ready,
    /// The server does not load the library yet, or the extension is missing in
    /// this database.
    Setup(StatementsSetup),
    Unavailable(String),
}

/// What `pg_stat_statements` still needs, and which steps Postern can take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatementsSetup {
    /// The running server loads the library.
    pub preloaded: bool,
    /// The extension exists in the current database.
    pub installed: bool,
    /// The server's configuration files load the library at the next start.
    pub configured: bool,
    /// Why Postern cannot change `shared_preload_libraries`, if it cannot.
    pub configure_blocker: Option<String>,
    pub restart: RestartSupport,
}

/// What the server reports about `shared_preload_libraries` and the
/// `pg_stat_statements` extension.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreloadFacts {
    /// The running value, when this login may read it.
    pub running: Option<Vec<String>>,
    /// The value in the configuration files; superusers only.
    pub configured: Option<ConfiguredPreload>,
    pub superuser: bool,
    /// The server has the extension's files.
    pub available: bool,
    /// The extension exists in the current database.
    pub installed: bool,
    /// Superusers only.
    pub data_directory: Option<String>,
    /// Directories searched for shared libraries; superusers only.
    pub library_directories: Vec<String>,
    pub backend_pid: i32,
}

/// The last `shared_preload_libraries` entry in the configuration files, which
/// applies at the next start unless the command line overrides it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfiguredPreload {
    pub libraries: Vec<String>,
    /// The entry is in `postgresql.auto.conf`, written by ALTER SYSTEM.
    pub in_auto_conf: bool,
}

/// Whether Postern can restart the server it is connected to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestartSupport {
    /// The server runs on this computer under the current user, started with
    /// `pg_ctl`, so Postern can stop and start it.
    Available {
        data_directory: String,
        log_file: String,
        /// Client sessions of other applications that a restart disconnects.
        other_sessions: i64,
    },
    Unavailable(String),
}

impl Default for RestartSupport {
    fn default() -> Self {
        Self::Unavailable(String::new())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlowQueryReport {
    pub status: StatementsStatus,
    pub queries: Vec<SlowQuery>,
    pub stats_since: Option<String>,
    /// The server supports `EXPLAIN (GENERIC_PLAN)` for statements with `$n` parameters.
    pub generic_plans: bool,
}
