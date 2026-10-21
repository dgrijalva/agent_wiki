//! Host entity types: records a host extracts from each document, each type
//! stored in its own table with a schema derived from the type
//! (`#[derive(Entity)]`).

use anyhow::{Result, bail};

use crate::index::fnv1a;

/// A type of record a host extracts from documents and the index stores;
/// usually derived. Registered with
/// [`WikiBuilder::entity`](crate::WikiBuilder::entity).
pub trait Entity: Send + 'static {
    /// Names the type's tables, `entity_{NAME}`: lowercase letters, digits
    /// and `_`.
    const NAME: &'static str;
    /// Bump it when what the records hold changes without the fields
    /// changing, such as an extraction rule, so stored records are rebuilt.
    const VERSION: u32;

    /// The fields, in the order [`values`](Entity::values) gives them.
    fn fields() -> Vec<Field>;

    fn values(&self) -> Vec<FieldValue>;
}

/// A field of an entity type, as a column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: &'static str,
    pub kind: FieldKind,
    /// May be NULL.
    pub optional: bool,
    /// Indexed with a b-tree, for exact and ordered lookups.
    pub index: bool,
    /// In the type's full-text index. Text fields only.
    pub fulltext: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Integer,
    Real,
    Bool,
}

/// A field's value in a record.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Null,
    Text(String),
    Integer(i64),
    Real(f64),
}

/// A Rust type an entity field can have.
pub trait EntityField {
    const KIND: FieldKind;
    const OPTIONAL: bool = false;

    fn value(&self) -> FieldValue;
}

impl EntityField for String {
    const KIND: FieldKind = FieldKind::Text;

    fn value(&self) -> FieldValue {
        FieldValue::Text(self.clone())
    }
}

impl EntityField for bool {
    const KIND: FieldKind = FieldKind::Bool;

    fn value(&self) -> FieldValue {
        FieldValue::Integer(i64::from(*self))
    }
}

macro_rules! integer_fields {
    ($($t:ty),*) => {$(
        impl EntityField for $t {
            const KIND: FieldKind = FieldKind::Integer;

            /// Values past `i64::MAX` are stored as `i64::MAX`.
            #[allow(clippy::unnecessary_fallible_conversions)]
            fn value(&self) -> FieldValue {
                FieldValue::Integer(i64::try_from(*self).unwrap_or(i64::MAX))
            }
        }
    )*};
}

integer_fields!(i8, i16, i32, i64, u8, u16, u32, u64, usize);

impl EntityField for f64 {
    const KIND: FieldKind = FieldKind::Real;

    fn value(&self) -> FieldValue {
        FieldValue::Real(*self)
    }
}

impl EntityField for f32 {
    const KIND: FieldKind = FieldKind::Real;

    fn value(&self) -> FieldValue {
        FieldValue::Real(f64::from(*self))
    }
}

impl<T: EntityField> EntityField for Option<T> {
    const KIND: FieldKind = T::KIND;
    const OPTIONAL: bool = true;

    fn value(&self) -> FieldValue {
        match self {
            Some(value) => value.value(),
            None => FieldValue::Null,
        }
    }
}

/// The records extracted from one document; see
/// [`Host::extract`](crate::Host::extract).
#[derive(Default)]
pub struct Records {
    pub(crate) records: Vec<(&'static str, Vec<FieldValue>)>,
}

impl Records {
    /// Adds a record. One of a type that isn't registered is logged and
    /// dropped when the document is stored.
    pub fn push<T: Entity>(&mut self, record: T) {
        self.records.push((T::NAME, record.values()));
    }
}

/// A registered entity type, as the index needs it.
#[derive(Debug, Clone)]
pub(crate) struct EntitySpec {
    pub(crate) name: &'static str,
    version: u32,
    pub(crate) fields: Vec<Field>,
}

impl EntitySpec {
    pub(crate) fn of<T: Entity>() -> Result<EntitySpec> {
        let spec = EntitySpec {
            name: T::NAME,
            version: T::VERSION,
            fields: T::fields(),
        };
        let valid = |name: &str| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        if !valid(spec.name) {
            bail!(
                "entity name {:?}: use lowercase letters, digits and _",
                spec.name
            );
        }
        for field in &spec.fields {
            if matches!(field.name, "id" | "page" | "ordinal") {
                bail!(
                    "entity {}: field name {:?} is reserved",
                    spec.name,
                    field.name
                );
            }
            if field.fulltext && field.kind != FieldKind::Text {
                bail!(
                    "entity {}: full-text field {} isn't text",
                    spec.name,
                    field.name
                );
            }
        }
        Ok(spec)
    }

    pub(crate) fn table(&self) -> String {
        format!("entity_{}", self.name)
    }

    pub(crate) fn fts_table(&self) -> String {
        format!("entity_{}_fts", self.name)
    }

    fn fulltext(&self) -> Vec<&Field> {
        self.fields.iter().filter(|f| f.fulltext).collect()
    }

    /// The statements creating the type's tables, indexes and triggers. A
    /// record belongs to a page; deleting the page deletes its records.
    pub(crate) fn create(&self) -> Vec<String> {
        let table = self.table();
        let mut columns = vec![
            "id INTEGER PRIMARY KEY".to_string(),
            "page INTEGER NOT NULL REFERENCES page(id) ON DELETE CASCADE".to_string(),
            "ordinal INTEGER NOT NULL".to_string(),
        ];
        for field in &self.fields {
            let ty = match field.kind {
                FieldKind::Text => "TEXT",
                FieldKind::Integer | FieldKind::Bool => "INTEGER",
                FieldKind::Real => "REAL",
            };
            let null = if field.optional { "" } else { " NOT NULL" };
            columns.push(format!("{} {ty}{null}", quote(field.name)));
        }
        let mut sql = vec![
            format!("CREATE TABLE {table} ({})", columns.join(", ")),
            format!("CREATE INDEX {table}_page ON {table}(page)"),
        ];
        for field in self.fields.iter().filter(|f| f.index) {
            sql.push(format!(
                "CREATE INDEX {} ON {table}({})",
                quote(&format!("{table}_{}", field.name)),
                quote(field.name)
            ));
        }
        let text = self.fulltext();
        if !text.is_empty() {
            let fts = self.fts_table();
            let cols: Vec<String> = text.iter().map(|f| quote(f.name)).collect();
            let cols = cols.join(", ");
            let new: Vec<String> = text
                .iter()
                .map(|f| format!("new.{}", quote(f.name)))
                .collect();
            let old: Vec<String> = text
                .iter()
                .map(|f| format!("old.{}", quote(f.name)))
                .collect();
            let (new, old) = (new.join(", "), old.join(", "));
            sql.push(format!(
                "CREATE VIRTUAL TABLE {fts} USING fts5({cols}, content='{table}', \
                 content_rowid='id', tokenize='unicode61 remove_diacritics 2')"
            ));
            sql.push(format!(
                "CREATE TRIGGER {table}_ai AFTER INSERT ON {table} BEGIN \
                 INSERT INTO {fts}(rowid, {cols}) VALUES (new.id, {new}); END"
            ));
            sql.push(format!(
                "CREATE TRIGGER {table}_ad AFTER DELETE ON {table} BEGIN \
                 INSERT INTO {fts}({fts}, rowid, {cols}) VALUES ('delete', old.id, {old}); END"
            ));
            sql.push(format!(
                "CREATE TRIGGER {table}_au AFTER UPDATE ON {table} BEGIN \
                 INSERT INTO {fts}({fts}, rowid, {cols}) VALUES ('delete', old.id, {old}); \
                 INSERT INTO {fts}(rowid, {cols}) VALUES (new.id, {new}); END"
            ));
        }
        sql
    }

    /// A hash of the type's statements and its version: stored records
    /// whose version differs are rebuilt.
    pub(crate) fn version(&self) -> i64 {
        let text = format!("{}\n{}", self.version, self.create().join(";\n"));
        (fnv1a(&text) & 0x7fff_ffff_ffff_ffff) as i64
    }

    /// `INSERT` of one record, with `?1` the page, `?2` the ordinal and the
    /// fields after.
    pub(crate) fn insert(&self) -> String {
        let names: Vec<String> = self.fields.iter().map(|f| quote(f.name)).collect();
        let params: Vec<String> = (0..self.fields.len())
            .map(|n| format!("?{}", n + 3))
            .collect();
        let (names, params) = if names.is_empty() {
            (String::new(), String::new())
        } else {
            (
                format!(", {}", names.join(", ")),
                format!(", {}", params.join(", ")),
            )
        };
        format!(
            "INSERT INTO {} (page, ordinal{names}) VALUES (?1, ?2{params})",
            self.table()
        )
    }
}

/// `name` as an SQL identifier.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ticket {
        id: String,
        status: Option<String>,
        done: bool,
    }

    impl Entity for Ticket {
        const NAME: &'static str = "ticket";
        const VERSION: u32 = 0;

        fn fields() -> Vec<Field> {
            vec![
                Field {
                    name: "key",
                    kind: <String as EntityField>::KIND,
                    optional: <String as EntityField>::OPTIONAL,
                    index: true,
                    fulltext: false,
                },
                Field {
                    name: "status",
                    kind: <Option<String> as EntityField>::KIND,
                    optional: <Option<String> as EntityField>::OPTIONAL,
                    index: false,
                    fulltext: true,
                },
                Field {
                    name: "done",
                    kind: <bool as EntityField>::KIND,
                    optional: false,
                    index: false,
                    fulltext: false,
                },
            ]
        }

        fn values(&self) -> Vec<FieldValue> {
            vec![self.id.value(), self.status.value(), self.done.value()]
        }
    }

    #[test]
    fn schema_from_fields() {
        let spec = EntitySpec::of::<Ticket>().unwrap();
        let sql = spec.create();
        assert_eq!(
            sql[0],
            "CREATE TABLE entity_ticket (id INTEGER PRIMARY KEY, \
             page INTEGER NOT NULL REFERENCES page(id) ON DELETE CASCADE, \
             ordinal INTEGER NOT NULL, \"key\" TEXT NOT NULL, \"status\" TEXT, \
             \"done\" INTEGER NOT NULL)"
        );
        assert!(
            sql.contains(
                &"CREATE INDEX \"entity_ticket_key\" ON entity_ticket(\"key\")".to_string()
            )
        );
        assert!(sql.iter().any(|s| {
            s.starts_with("CREATE VIRTUAL TABLE entity_ticket_fts USING fts5(\"status\"")
        }));
        assert_eq!(
            spec.insert(),
            "INSERT INTO entity_ticket (page, ordinal, \"key\", \"status\", \"done\") \
             VALUES (?1, ?2, ?3, ?4, ?5)"
        );
        let record = Ticket {
            id: "ABC-1".into(),
            status: None,
            done: true,
        };
        assert_eq!(
            record.values(),
            [
                FieldValue::Text("ABC-1".into()),
                FieldValue::Null,
                FieldValue::Integer(1)
            ]
        );
    }
}
