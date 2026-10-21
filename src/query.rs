//! Record queries over entity types: `wiki.query::<E>()`, returning each
//! matching record with its page. Built with sea-query, which stays private.

use std::marker::PhantomData;

use anyhow::{Result, bail};
use sea_query::extension::sqlite::SqliteExpr;
use sea_query::{Expr, ExprTrait, Order, Query as Select, SelectStatement, SqliteQueryBuilder};

use crate::Wiki;
use crate::entity::{EntitySpec, EntityType, FieldValue};
use crate::parse::Host;

/// A condition on an entity type's fields, by name; see the functions in
/// [`filter`](crate::filter). Field names are checked when the query runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter(Node);

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Compare(String, Op, FieldValue),
    In(String, Vec<FieldValue>, bool),
    Between(String, FieldValue, FieldValue, bool),
    Null(String, bool),
    Matches(String, String),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    Not(Box<Node>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    Like,
    NotLike,
    Glob,
}

impl Filter {
    pub fn and(self, other: Filter) -> Filter {
        Filter(Node::And(Box::new(self.0), Box::new(other.0)))
    }

    pub fn or(self, other: Filter) -> Filter {
        Filter(Node::Or(Box::new(self.0), Box::new(other.0)))
    }

    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Filter {
        Filter(Node::Not(Box::new(self.0)))
    }

    /// The condition as an expression on `spec`'s table.
    fn compile(&self, spec: &EntitySpec) -> Result<Expr> {
        compile(&self.0, spec)
    }
}

/// The filter functions: every field-against-value condition, and
/// `matches` for full-text fields. A value is anything `Into<FieldValue>`;
/// a string is always a value, never a field.
pub mod filter {
    use super::{Filter, Node, Op};
    use crate::entity::FieldValue;

    fn compare(field: &str, op: Op, value: impl Into<FieldValue>) -> Filter {
        Filter(Node::Compare(field.to_string(), op, value.into()))
    }

    pub fn eq(field: &str, value: impl Into<FieldValue>) -> Filter {
        compare(field, Op::Eq, value)
    }

    pub fn ne(field: &str, value: impl Into<FieldValue>) -> Filter {
        compare(field, Op::Ne, value)
    }

    pub fn lt(field: &str, value: impl Into<FieldValue>) -> Filter {
        compare(field, Op::Lt, value)
    }

    pub fn lte(field: &str, value: impl Into<FieldValue>) -> Filter {
        compare(field, Op::Lte, value)
    }

    pub fn gt(field: &str, value: impl Into<FieldValue>) -> Filter {
        compare(field, Op::Gt, value)
    }

    pub fn gte(field: &str, value: impl Into<FieldValue>) -> Filter {
        compare(field, Op::Gte, value)
    }

    /// SQL `LIKE`: `%` and `_` are wildcards; case-insensitive for ASCII.
    pub fn like(field: &str, pattern: impl Into<String>) -> Filter {
        compare(field, Op::Like, pattern.into())
    }

    pub fn not_like(field: &str, pattern: impl Into<String>) -> Filter {
        compare(field, Op::NotLike, pattern.into())
    }

    /// SQLite `GLOB`: `*` and `?` are wildcards; case-sensitive.
    pub fn glob(field: &str, pattern: impl Into<String>) -> Filter {
        compare(field, Op::Glob, pattern.into())
    }

    pub fn is_in<V: Into<FieldValue>>(field: &str, values: impl IntoIterator<Item = V>) -> Filter {
        let values = values.into_iter().map(Into::into).collect();
        Filter(Node::In(field.to_string(), values, false))
    }

    pub fn is_not_in<V: Into<FieldValue>>(
        field: &str,
        values: impl IntoIterator<Item = V>,
    ) -> Filter {
        let values = values.into_iter().map(Into::into).collect();
        Filter(Node::In(field.to_string(), values, true))
    }

    /// Inclusive at both ends.
    pub fn between(field: &str, low: impl Into<FieldValue>, high: impl Into<FieldValue>) -> Filter {
        Filter(Node::Between(
            field.to_string(),
            low.into(),
            high.into(),
            false,
        ))
    }

    pub fn not_between(
        field: &str,
        low: impl Into<FieldValue>,
        high: impl Into<FieldValue>,
    ) -> Filter {
        Filter(Node::Between(
            field.to_string(),
            low.into(),
            high.into(),
            true,
        ))
    }

    pub fn is_null(field: &str) -> Filter {
        Filter(Node::Null(field.to_string(), false))
    }

    pub fn is_not_null(field: &str) -> Filter {
        Filter(Node::Null(field.to_string(), true))
    }

    /// A full-text match on a `fulltext` field, in FTS5 query syntax.
    pub fn matches(field: &str, query: impl Into<String>) -> Filter {
        Filter(Node::Matches(field.to_string(), query.into()))
    }
}

/// What results are ordered by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sort {
    /// The page's edit time.
    Edited,
    /// The page's path.
    Path,
    /// The record's place among its type's records on the page.
    Ordinal,
    /// One of the entity type's fields.
    Field(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Asc,
    Desc,
}

/// A record with the page it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity<E> {
    id: i64,
    ordinal: i64,
    path: String,
    url: String,
    title: String,
    folder: String,
    edited: i64,
    record: E,
}

impl<E> Entity<E> {
    /// The record's id, unique in its type's table while its page is
    /// indexed.
    pub fn id(&self) -> i64 {
        self.id
    }

    /// Its place among its type's records on the page, from 0.
    pub fn ordinal(&self) -> i64 {
        self.ordinal
    }

    /// The page's path from the root, e.g. `Notes/Daily.md`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The page's URL path under the base, e.g. `/Notes/Daily`.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// The folder holding the page; `""` for the root.
    pub fn folder(&self) -> &str {
        &self.folder
    }

    /// When the page was last edited, in seconds since the epoch.
    pub fn edited(&self) -> i64 {
        self.edited
    }

    pub fn record(&self) -> &E {
        &self.record
    }

    pub fn into_record(self) -> E {
        self.record
    }
}

/// A record query being built; from [`Wiki::query`].
pub struct Query<P: Host, E: EntityType> {
    wiki: Wiki<P>,
    filters: Vec<Filter>,
    under: Option<String>,
    document: Option<String>,
    on_pages_with: Vec<(&'static str, Filter)>,
    order: Vec<(Sort, Direction)>,
    limit: Option<u64>,
    offset: Option<u64>,
    entity: PhantomData<fn() -> E>,
}

impl<P: Host> Wiki<P> {
    /// A query for the records of entity type `E`, which must be
    /// registered.
    pub fn query<E: EntityType>(&self) -> Query<P, E> {
        Query {
            wiki: self.clone(),
            filters: Vec::new(),
            under: None,
            document: None,
            on_pages_with: Vec::new(),
            order: Vec::new(),
            limit: None,
            offset: None,
            entity: PhantomData,
        }
    }
}

impl<P: Host, E: EntityType> Query<P, E> {
    /// Keeps records matching `filter`; with several, all must match.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Keeps records on pages in `folder` or below it (`""` is the root).
    pub fn under(mut self, folder: &str) -> Self {
        self.under = Some(folder.trim_matches('/').to_string());
        self
    }

    /// Keeps records of the document at `path` (from the root, `.md`
    /// optional).
    pub fn document(mut self, path: &str) -> Self {
        let path = path.trim_matches('/');
        self.document = Some(if path.ends_with(".md") {
            path.to_string()
        } else {
            format!("{path}.md")
        });
        self
    }

    /// Keeps records on pages that have a record of type `T` matching
    /// `filter`.
    pub fn on_pages_with<T: EntityType>(mut self, filter: Filter) -> Self {
        self.on_pages_with.push((T::NAME, filter));
        self
    }

    /// Orders by `sort`, after any earlier orderings. Without any: the
    /// page's path, then the record's ordinal.
    pub fn order_by(mut self, sort: Sort, direction: Direction) -> Self {
        self.order.push((sort, direction));
        self
    }

    pub fn limit(mut self, n: u64) -> Self {
        self.limit = Some(n);
        self
    }

    pub fn offset(mut self, n: u64) -> Self {
        self.offset = Some(n);
        self
    }

    /// Runs the query. Fails for a type that isn't registered, an unknown
    /// field, or `matches` on a field that isn't full-text.
    pub async fn fetch(self) -> Result<Vec<Entity<E>>> {
        let index = &self.wiki.inner.index;
        let spec = index.entity(E::NAME)?;
        let (sql, values) = self.statement(spec, |name| index.entity(name))?;
        let rows = index.select(&sql, values).await?;
        rows.into_iter().map(decode).collect()
    }

    fn statement<'a>(
        &self,
        spec: &EntitySpec,
        lookup: impl Fn(&str) -> Result<&'a EntitySpec>,
    ) -> Result<(String, Vec<FieldValue>)> {
        let table = spec.table();
        let mut q = Select::select();
        for column in ["id", "ordinal"] {
            q.column((table.clone(), column));
        }
        for column in ["path", "url", "title", "folder", "edited"] {
            q.column(("page", column));
        }
        for field in &spec.fields {
            q.column((table.clone(), field.name));
        }
        q.from(table.clone()).inner_join(
            "page",
            Expr::col(("page", "id")).equals((table.clone(), "page")),
        );
        if let Some(folder) = self.under.as_deref().filter(|f| !f.is_empty()) {
            q.and_where(Expr::col(("page", "path")).gte(format!("{folder}/")))
                .and_where(Expr::col(("page", "path")).lt(format!("{folder}0")));
        }
        if let Some(path) = &self.document {
            q.and_where(Expr::col(("page", "path")).eq(path.clone()));
        }
        for filter in &self.filters {
            q.and_where(filter.compile(spec)?);
        }
        for (name, filter) in &self.on_pages_with {
            let other = lookup(name)?;
            let mut pages: SelectStatement = Select::select();
            pages
                .column((other.table(), "page"))
                .from(other.table())
                .and_where(filter.compile(other)?);
            q.and_where(Expr::col((table.clone(), "page")).in_subquery(pages));
        }
        let order = if self.order.is_empty() {
            vec![
                (Sort::Path, Direction::Asc),
                (Sort::Ordinal, Direction::Asc),
            ]
        } else {
            self.order.clone()
        };
        for (sort, direction) in order {
            let column = match &sort {
                Sort::Edited => Expr::col(("page", "edited")),
                Sort::Path => Expr::col(("page", "path")),
                Sort::Ordinal => Expr::col((table.clone(), "ordinal")),
                Sort::Field(name) => column(spec, name)?,
            };
            let direction = match direction {
                Direction::Asc => Order::Asc,
                Direction::Desc => Order::Desc,
            };
            q.order_by_expr(column, direction);
        }
        // Ties broken the same way every time.
        q.order_by((table.clone(), "id"), Order::Asc);
        if let Some(n) = self.limit {
            q.limit(n);
        }
        if let Some(n) = self.offset {
            q.offset(n);
        }
        let (sql, values) = q.build(SqliteQueryBuilder);
        Ok((sql, values.into_iter().map(from_sea).collect()))
    }
}

/// The column of field `name` of `spec`'s table.
fn column(spec: &EntitySpec, name: &str) -> Result<Expr> {
    if spec.field(name).is_none() {
        bail!("entity {} has no field {name:?}", spec.name);
    }
    Ok(Expr::col((spec.table(), name.to_string())))
}

fn compile(node: &Node, spec: &EntitySpec) -> Result<Expr> {
    Ok(match node {
        Node::Compare(field, op, value) => {
            let col = column(spec, field)?;
            let value = to_sea(value);
            match op {
                Op::Eq => col.eq(value),
                Op::Ne => col.ne(value),
                Op::Lt => col.lt(value),
                Op::Lte => col.lte(value),
                Op::Gt => col.gt(value),
                Op::Gte => col.gte(value),
                Op::Like => col.like(text(value)?),
                Op::NotLike => col.not_like(text(value)?),
                Op::Glob => col.glob(value),
            }
        }
        Node::In(field, values, negate) => {
            let col = column(spec, field)?;
            let values = values.iter().map(to_sea);
            if *negate {
                col.is_not_in(values)
            } else {
                col.is_in(values)
            }
        }
        Node::Between(field, low, high, negate) => {
            let col = column(spec, field)?;
            if *negate {
                col.not_between(to_sea(low), to_sea(high))
            } else {
                col.between(to_sea(low), to_sea(high))
            }
        }
        Node::Null(field, negate) => {
            let col = column(spec, field)?;
            if *negate {
                col.is_not_null()
            } else {
                col.is_null()
            }
        }
        Node::Matches(field, query) => {
            let Some(f) = spec.field(field) else {
                bail!("entity {} has no field {field:?}", spec.name);
            };
            if !f.fulltext {
                bail!("entity {}: field {field} isn't full-text", spec.name);
            }
            // A column on the left of MATCH limits the match to it.
            let fts = spec.fts_table();
            let mut rows = Select::select();
            rows.expr(Expr::cust("rowid"))
                .from(fts.clone())
                .and_where(Expr::col((fts, field.clone())).matches(query.clone()));
            Expr::col((spec.table(), "id")).in_subquery(rows)
        }
        Node::And(a, b) => compile(a, spec)?.and(compile(b, spec)?),
        Node::Or(a, b) => compile(a, spec)?.or(compile(b, spec)?),
        Node::Not(a) => compile(a, spec)?.not(),
    })
}

fn text(value: sea_query::Value) -> Result<String> {
    match value {
        sea_query::Value::String(Some(text)) => Ok(text),
        other => bail!("a pattern must be text, not {other:?}"),
    }
}

fn to_sea(value: &FieldValue) -> sea_query::Value {
    match value {
        FieldValue::Null => sea_query::Value::BigInt(None),
        FieldValue::Text(text) => text.clone().into(),
        FieldValue::Integer(n) => (*n).into(),
        FieldValue::Real(x) => (*x).into(),
    }
}

/// A sea-query value as one of ours. Other crates can enable sea-query
/// features that add variants, hence the catch-all.
#[allow(unreachable_patterns)]
fn from_sea(value: sea_query::Value) -> FieldValue {
    use sea_query::Value as V;
    let int = |n: Option<i64>| n.map_or(FieldValue::Null, FieldValue::Integer);
    match value {
        V::Bool(b) => int(b.map(i64::from)),
        V::TinyInt(n) => int(n.map(i64::from)),
        V::SmallInt(n) => int(n.map(i64::from)),
        V::Int(n) => int(n.map(i64::from)),
        V::BigInt(n) => int(n),
        V::TinyUnsigned(n) => int(n.map(i64::from)),
        V::SmallUnsigned(n) => int(n.map(i64::from)),
        V::Unsigned(n) => int(n.map(i64::from)),
        V::BigUnsigned(n) => int(n.map(|n| i64::try_from(n).unwrap_or(i64::MAX))),
        V::Float(x) => x.map_or(FieldValue::Null, |x| FieldValue::Real(f64::from(x))),
        V::Double(x) => x.map_or(FieldValue::Null, FieldValue::Real),
        V::String(s) => s.map_or(FieldValue::Null, FieldValue::Text),
        V::Char(c) => c.map_or(FieldValue::Null, |c| FieldValue::Text(c.to_string())),
        other => unreachable!("agent_wiki never binds {other:?}"),
    }
}

/// A result row: the record's id and ordinal, the page's columns, then the
/// fields.
fn decode<E: EntityType>(row: Vec<FieldValue>) -> Result<Entity<E>> {
    let mut row = row.into_iter();
    let mut next = || row.next().unwrap_or(FieldValue::Null);
    let int = |v: FieldValue| match v {
        FieldValue::Integer(n) => Ok(n),
        other => bail!("expected an integer, found {other:?}"),
    };
    let text = |v: FieldValue| match v {
        FieldValue::Text(t) => Ok(t),
        other => bail!("expected text, found {other:?}"),
    };
    let id = int(next())?;
    let ordinal = int(next())?;
    let path = text(next())?;
    let url = text(next())?;
    let title = text(next())?;
    let folder = text(next())?;
    let edited = int(next())?;
    let record = E::from_values(row.collect())?;
    Ok(Entity {
        id,
        ordinal,
        path,
        url,
        title,
        folder,
        edited,
        record,
    })
}
