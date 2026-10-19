// Sketch of the host API. Not compiled.
// See docs/design/agent_wiki.md → Host API → Parsing and extraction.
//
// The core move: a node's *kind* holds no children, and children are an
// opaque library type. The API never commits to nesting; storage is an arena
// (daggy or equivalent) behind it.
//
// Every public type is parameterized by the host `P`, which names the host's
// block and inline element types and its page template.

// ─── Node kinds: what a node is, without its children ───────────────────────

pub enum BlockKind<P: Host> {
    Paragraph,
    Heading { level: u8 },
    Quote,
    List { ordered: bool },
    ListItem { task: Option<bool> },
    Code { info: String, body: String },
    Html(String),
    // …
    Host(P::Block),
}

pub enum InlineKind<P: Host> {
    Text(String),
    Strong,
    Emph,
    Code(String),
    Link { dest: String, title: Option<String> },
    WikiLink { target: String },
    Tag(String),
    // …
    Host(P::Inline),
}

// ─── Children: opaque, owned by the library ─────────────────────────────────

/// A range of nodes in the library's arena. Hosts get these from the stream
/// and hand them back; they never build or open one.
pub struct Children<K>(/* … */);

/// What a block holds.
pub enum Content<P: Host> {
    Blocks(Children<BlockKind<P>>),
    Inlines(Children<InlineKind<P>>),
    Leaf,
}

/// A node the stream built for the host; returned from a parse hook.
pub struct Parsed<K>(/* … */);

// ─── The host's parser ──────────────────────────────────────────────────────

pub trait Host: Sized + Send + Sync + 'static {
    type Block: Element<Self>;
    type Inline: Element<Self>;
    type Template: Template<Self>;

    /// Called at the start of every line, including inside containers and
    /// mid-paragraph (a claim ends the paragraph). `None` = not mine: the
    /// library rewinds the stream and tries its built-ins.
    fn parse_block(&self, s: &mut BlockStream<'_, Self>) -> Option<Parsed<BlockKind<Self>>> {
        None
    }

    /// Called at each position in an inline section, after escapes and code
    /// spans, before the built-ins. Same contract.
    fn parse_inline(&self, s: &mut InlineStream<'_, Self>) -> Option<Parsed<InlineKind<Self>>> {
        None
    }
}

/// The default: a wiki with no host syntax.
pub struct Plain;
pub enum Never {}

impl Host for Plain {
    type Block = Never;
    type Inline = Never;
    type Template = DefaultTemplate;
}

// ─── Streams: nom in, library nesting out ───────────────────────────────────

/// Holds `&P`, so nested parsing goes back through the host first.
pub struct InlineStream<'a, P: Host> { /* input, &'a P, … */ }

impl<P: Host> InlineStream<'_, P> {
    /// Run a nom parser at the current position; advance on success.
    pub fn try_parse<O>(&mut self, p: impl nom::Parser<&str, Output = O, Error = …>) -> Option<O>;

    /// Library → host → library: parse inline content (host first, then
    /// built-ins) until `end` matches; consumes `end`. Emphasis is resolved
    /// within the returned children, so it never crosses a host boundary.
    pub fn inlines_until<O>(
        &mut self,
        end: impl nom::Parser<&str, Output = O, Error = …>,
    ) -> Option<Children<InlineKind<P>>>;

    pub fn node(&mut self, kind: InlineKind<P>, children: Children<InlineKind<P>>) -> Parsed<InlineKind<P>>;
    pub fn leaf(&mut self, kind: InlineKind<P>) -> Parsed<InlineKind<P>>;
}

pub struct BlockStream<'a, P: Host> { /* lines with container prefixes stripped, &'a P, … */ }

impl<P: Host> BlockStream<'_, P> {
    pub fn try_parse<O>(&mut self, p: impl nom::Parser<&str, Output = O, Error = …>) -> Option<O>;

    /// The current line, container prefixes (`> `, list indentation) stripped.
    pub fn line(&self) -> &str;

    /// Parse blocks (host first, then built-ins) until `end` matches at a
    /// line start; consumes `end`.
    pub fn blocks_until<O>(
        &mut self,
        end: impl nom::Parser<&str, Output = O, Error = …>,
    ) -> Option<Children<BlockKind<P>>>;

    /// Parse `text` as an inline section, e.g. a host block's title line.
    pub fn inlines_in(&mut self, text: &str) -> Children<InlineKind<P>>;

    pub fn node(&mut self, kind: BlockKind<P>, content: Content<P>) -> Parsed<BlockKind<P>>;
}

// ─── Drawing: parsing in reverse ────────────────────────────────────────────

/// Host elements are plain data that draw themselves around their children.
pub trait Element<P: Host>: Sized {
    /// `children` draws the node's children (maud `Render`), so it drops into
    /// `html!` as `(children)`. `cx.base_path()` is where the wiki is mounted.
    fn draw(&self, cx: &DrawCtx<'_>, children: DrawChildren<'_, P>) -> maud::Markup;

    /// Text this element contributes to search, besides its children's.
    fn text(&self) -> Option<String> { None }
}

/// `base_path()`: no trailing slash, `""` at the root.
pub struct DrawCtx<'a> { /* … */ }

pub struct DrawChildren<'a, P: Host> { /* … */ }
impl<P: Host> maud::Render for DrawChildren<'_, P> { /* … */ }

// ─── The page: the library owns the skeleton, the host fills it ────────────
// Hooks return `impl Future + Send` (axum needs Send futures); hosts write
// `async fn` in their impls.

pub trait Template<P: Host>: Send + Sync + 'static {
    /// Appended to the library's <head>. `cx` can `set_title(…)`,
    /// `suppress_default_styles()`.
    fn head(&self, cx: &mut HeadCx<'_, P>) -> impl Future<Output = maud::Markup> + Send;

    /// Wrapped in the htmx root. Must embed `content(cx)`. A host that
    /// overrides this places the built-in chrome itself (`cx.search_box()`,
    /// `cx.breadcrumbs()`, `cx.tags()`, `cx.frontmatter()`).
    fn body(&self, cx: &PageCx<'_, P>, content: PageContent<'_, P>)
        -> impl Future<Output = maud::Markup> + Send;
}

/// Today's page.
pub struct DefaultTemplate;

// PageCx: kind (Document { meta, frontmatter } | Folder | Search { query } |
//         Tag | Disambiguation | NotFound), request(), chrome accessors.

// ─── Reading and editing: handles and methods, never public fields ──────────
// Editing is not needed for the first host integration.

pub struct NodeRef<'a, P: Host> { /* arena id */ }

impl<'a, P: Host> NodeRef<'a, P> {
    pub fn kind(&self) -> KindRef<'a, P>; // &BlockKind<P> or &InlineKind<P>
    pub fn children(&self) -> impl Iterator<Item = NodeRef<'a, P>>;
    pub fn parent(&self) -> Option<NodeRef<'a, P>>;
    pub fn next_sibling(&self) -> Option<NodeRef<'a, P>>;
}

// ─── Example: a host with tickets and markers ───────────────────────────────

struct SampleHost;

enum SampleInline {
    Ticket(String),
    /// A marker's body is its children, held by the library.
    Marker { kind: MarkerKind },
}

impl Host for SampleHost {
    type Block = Never;
    type Inline = SampleInline;
    type Template = DefaultTemplate;

    fn parse_inline(&self, s: &mut InlineStream<'_, Self>) -> Option<Parsed<InlineKind<Self>>> {
        // [[ABC-1234]] — a ticket, not a wikilink
        if let Some(id) = s.try_parse(delimited(tag("[["), linear_id, tag("]]"))) {
            return Some(s.leaf(InlineKind::Host(SampleInline::Ticket(id))));
        }
        // [OPEN: **foo** text with inline _formatting_]
        if let Some(kind) = s.try_parse(delimited(tag("["), marker_kind, tag(": "))) {
            let body = s.inlines_until(tag("]"))?; // None → whole call rewinds
            return Some(s.node(InlineKind::Host(SampleInline::Marker { kind }), body));
        }
        None
    }
}

impl Element<SampleHost> for SampleInline {
    fn draw(&self, _cx: &DrawCtx<'_>, children: DrawChildren<'_, SampleHost>) -> maud::Markup {
        match self {
            SampleInline::Ticket(id) => html! {
                // Live status: htmx fragment from the host, or the host's own toolkit (e.g. Alpine).
                span.ticket hx-get={ "/api/linear/" (id) } hx-trigger="load" { (id) }
            },
            SampleInline::Marker { kind } => html! {
                span class={ "marker marker-" (kind) } { (children) }
            },
        }
    }
}
