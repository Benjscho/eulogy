//! Derive macro for [`eulogy::AsyncDrop`].
//!
//! Don't depend on this crate directly. Enable the `derive` feature on
//! `eulogy`, which re-exports the macro:
//!
//! ```toml
//! eulogy = { version = "0.1", features = ["tokio", "derive"] }
//! ```
//!
//! See the [`AsyncDrop`] derive for the attribute reference and examples.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{format_ident, quote, ToTokens};
use syn::spanned::Spanned;
use syn::{
    parse_macro_input, parse_quote, Data, DeriveInput, Expr, ExprArray, Fields, Ident, Index, Lit,
    Member, Meta, Type, Variant, WhereClause,
};

/// Resolve the path to the `eulogy` crate, honoring any rename via
/// `[dependencies] foo = { package = "eulogy" }`.
fn eulogy_crate() -> TokenStream2 {
    match crate_name("eulogy") {
        // Inside eulogy itself, `crate::` won't resolve when the derive is
        // used from a doctest binary. Rely on `extern crate self as eulogy;`
        // in lib.rs so `::eulogy` works in both crate-internal and doctest
        // contexts.
        Ok(FoundCrate::Itself) => quote!(::eulogy),
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name, Span::call_site());
            quote!(::#ident)
        }
        // Fallback to the canonical name; will emit a normal "unresolved
        // crate `eulogy`" error if the user really hasn't depended on it.
        Err(_) => quote!(::eulogy),
    }
}

/// Derive `AsyncDrop` for a struct or enum.
///
/// Every field is dropped by default, and the generated `async_drop` runs
/// independent fields concurrently. Add `#[eulogy(after = [...])]` to
/// order fields, or `#[eulogy(skip)]` for fields whose types don't implement
/// `AsyncDrop`.
///
/// # Attributes
///
/// - `#[eulogy(after = [field, ...])]` — drop this field only after the
///   listed fields have finished. Fields with no `after` deps and no
///   dependents drop concurrently. Cycles are a compile error.
/// - `#[eulogy(skip)]` — leave this field to its normal sync `Drop`. Use for
///   any field whose type doesn't implement `AsyncDrop`.
///
/// # Structs
///
/// ```
/// use eulogy::AsyncDrop;
///
/// struct Socket { id: u64 }
/// impl AsyncDrop for Socket {
///     async fn async_drop(self) { /* close */ }
/// }
///
/// struct Logger;
/// impl AsyncDrop for Logger {
///     async fn async_drop(self) { /* flush */ }
/// }
///
/// #[derive(AsyncDrop)]
/// struct Connection {
///     socket: Socket,
///     // Flush the logger only after the socket has finished closing.
///     #[eulogy(after = [socket])]
///     logger: Logger,
/// }
/// ```
///
/// Tuple structs work the same way; reference fields by position:
///
/// ```
/// # use eulogy::AsyncDrop;
/// # struct Socket; impl AsyncDrop for Socket { async fn async_drop(self) {} }
/// # struct Logger; impl AsyncDrop for Logger { async fn async_drop(self) {} }
/// #[derive(AsyncDrop)]
/// struct Connection(Socket, #[eulogy(after = [0])] Logger);
/// ```
///
/// # Enums
///
/// Each variant is treated like its own struct body — `after` references are
/// scoped to the enclosing variant.
///
/// ```
/// # use eulogy::AsyncDrop;
/// # struct Socket; impl AsyncDrop for Socket { async fn async_drop(self) {} }
/// # struct Logger; impl AsyncDrop for Logger { async fn async_drop(self) {} }
/// #[derive(AsyncDrop)]
/// enum Connection {
///     Tcp {
///         sock: Socket,
///         #[eulogy(after = [sock])]
///         logger: Logger,
///     },
///     Unix(Socket),
///     Closed, // no fields — this arm is a no-op
/// }
/// ```
///
/// # Skipping fields
///
/// Types that don't implement `AsyncDrop` (e.g. third-party types, or your
/// own sync-only types) need `#[eulogy(skip)]`. They'll be dropped
/// synchronously via their normal `Drop` impl:
///
/// ```
/// # use eulogy::AsyncDrop;
/// # struct Socket; impl AsyncDrop for Socket { async fn async_drop(self) {} }
/// struct Metrics; // no AsyncDrop impl
///
/// #[derive(AsyncDrop)]
/// struct Connection {
///     socket: Socket,
///     #[eulogy(skip)]
///     metrics: Metrics,
/// }
/// ```
///
/// Most standard-library value types (`String`, integers, `PathBuf`,
/// `Option`, `Vec`, tuples up to 12, ...) already have a built-in
/// `AsyncDrop` impl, so they don't need `skip`. See the `eulogy` crate for
/// the full list.
///
/// # Generics
///
/// `T: AsyncDrop` bounds are added automatically for each non-skipped field
/// type — you don't need to spell them out:
///
/// ```
/// # use eulogy::AsyncDrop;
/// #[derive(AsyncDrop)]
/// struct Pair<A, B> {
///     first: A,
///     #[eulogy(after = [first])]
///     second: B,
/// }
/// ```
///
/// # Unions
///
/// Not supported — there's no safe way for the derive to know which field is
/// active, so it can't know what to drop. Attempting `#[derive(AsyncDrop)]`
/// on a union is a compile error.
#[proc_macro_derive(AsyncDrop, attributes(eulogy))]
pub fn derive_async_drop(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

struct Entry {
    member: Member,
    ty: Type,
    after: Vec<Member>,
}

/// Collapse Named / Unnamed / Unit fields into a single (Member, &Field) list.
fn collect_field_list(fields: &Fields) -> Vec<(Member, &syn::Field)> {
    match fields {
        Fields::Named(fields) => fields
            .named
            .iter()
            .map(|f| (Member::Named(f.ident.clone().expect("named field")), f))
            .collect(),
        Fields::Unnamed(fields) => fields
            .unnamed
            .iter()
            .enumerate()
            .map(|(i, f)| {
                (
                    Member::Unnamed(Index {
                        index: i as u32,
                        span: f.span(),
                    }),
                    f,
                )
            })
            .collect(),
        Fields::Unit => Vec::new(),
    }
}

/// Parse `#[eulogy(...)]` attributes for a field list, returning the entries
/// that should be async-dropped (i.e. not `#[eulogy(skip)]`).
fn parse_entries(field_list: &[(Member, &syn::Field)]) -> syn::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = Vec::new();

    for (member, f) in field_list.iter() {
        let ty = f.ty.clone();
        let attr = f.attrs.iter().find(|a| a.path().is_ident("eulogy"));

        let mut after: Vec<Member> = Vec::new();
        let mut skip = false;
        let mut skip_span: Option<Span> = None;
        let mut after_span: Option<Span> = None;

        if let Some(attr) = attr {
            if let Meta::List(_) = &attr.meta {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("after") {
                        after_span = Some(meta.path.span());
                        meta.input.parse::<syn::Token![=]>()?;
                        let arr: ExprArray = meta.input.parse()?;
                        for elem in &arr.elems {
                            match elem {
                                Expr::Path(p) => {
                                    if let Some(seg) = p.path.segments.last() {
                                        after.push(Member::Named(seg.ident.clone()));
                                    } else {
                                        return Err(syn::Error::new_spanned(
                                            elem,
                                            "expected a field name or index",
                                        ));
                                    }
                                }
                                Expr::Lit(lit_expr) => {
                                    if let Lit::Int(int) = &lit_expr.lit {
                                        let idx: u32 = int.base10_parse()?;
                                        after.push(Member::Unnamed(Index {
                                            index: idx,
                                            span: int.span(),
                                        }));
                                    } else {
                                        return Err(syn::Error::new_spanned(
                                            elem,
                                            "expected a field name or index",
                                        ));
                                    }
                                }
                                other => {
                                    return Err(syn::Error::new_spanned(
                                        other,
                                        "expected a field name or index",
                                    ));
                                }
                            }
                        }
                        Ok(())
                    } else if meta.path.is_ident("skip") {
                        skip = true;
                        skip_span = Some(meta.path.span());
                        Ok(())
                    } else {
                        Err(syn::Error::new_spanned(
                            &meta.path,
                            "unknown #[eulogy(...)] key — expected `after` or `skip`",
                        ))
                    }
                })?;
            }
        }

        if skip {
            if let (Some(skip_span), Some(_)) = (skip_span, after_span) {
                return Err(syn::Error::new(
                    skip_span,
                    "`skip` cannot be combined with `after` — pick one",
                ));
            }
            continue;
        }

        entries.push(Entry {
            member: member.clone(),
            ty,
            after,
        });
    }

    Ok(entries)
}

/// Validate every `after` reference within one struct/variant's field list:
/// it must name a field, that field must itself be annotated (not skipped),
/// and it must not be self-referential. `container_desc` names the container
/// for error messages (e.g. "this struct" or "variant `Bar`").
fn validate_after_refs(entries: &[Entry], all_members: &[Member], container_desc: &str) -> syn::Result<()> {
    for entry in entries {
        for dep in &entry.after {
            if dep == &entry.member {
                return Err(syn::Error::new_spanned(
                    dep,
                    format!(
                        "field `{}` cannot list itself in #[eulogy(after = [...])]",
                        dep.to_token_stream()
                    ),
                ));
            }
            if !all_members.iter().any(|m| m == dep) {
                return Err(syn::Error::new_spanned(
                    dep,
                    format!("no field `{}` in {container_desc}", dep.to_token_stream()),
                ));
            }
            if !entries.iter().any(|e| &e.member == dep) {
                return Err(syn::Error::new_spanned(
                    dep,
                    format!(
                        "field `{}` is skipped or unannotated — it cannot appear in an `after` list",
                        dep.to_token_stream()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Group entries into topological layers via Kahn's algorithm.
///
/// Layer 0 contains all entries with no `after` deps; layer N contains
/// entries whose deps are all in layers 0..N. Independent fields end up
/// in the same layer and can be dropped concurrently.
fn topo_layers(entries: &[Entry]) -> syn::Result<Vec<Vec<Member>>> {
    let n = entries.len();
    let mut remaining: Vec<usize> = entries.iter().map(|e| e.after.len()).collect();
    // Reverse edges: dep_idx -> [entries that depend on dep_idx]
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, e) in entries.iter().enumerate() {
        for dep in &e.after {
            let dep_idx = entries
                .iter()
                .position(|x| &x.member == dep)
                .expect("validated dep");
            children[dep_idx].push(i);
        }
    }

    let mut layers: Vec<Vec<Member>> = Vec::new();
    let mut placed = vec![false; n];
    let mut placed_count = 0;

    loop {
        let ready: Vec<usize> = (0..n)
            .filter(|&i| !placed[i] && remaining[i] == 0)
            .collect();
        if ready.is_empty() {
            break;
        }
        let members: Vec<Member> = ready.iter().map(|&i| entries[i].member.clone()).collect();
        for &i in &ready {
            placed[i] = true;
            placed_count += 1;
            for &child in &children[i] {
                remaining[child] -= 1;
            }
        }
        layers.push(members);
    }

    if placed_count != n {
        // At least one entry was never placed — it must be in a cycle.
        let stuck = (0..n).find(|&i| !placed[i]).expect("cycle exists");
        return Err(syn::Error::new_spanned(
            &entries[stuck].member,
            "cycle detected in #[eulogy(after = [...])] dependencies",
        ));
    }

    Ok(layers)
}

/// Emit the `.async_drop().await` calls for each topological layer. `accessor`
/// turns a `Member` into the expression used to reach that field's value —
/// `self.field` for a struct, or a locally bound identifier inside an enum
/// match arm.
fn layer_calls(
    layers: &[Vec<Member>],
    krate: &TokenStream2,
    accessor: impl Fn(&Member) -> TokenStream2,
) -> Vec<TokenStream2> {
    layers
        .iter()
        .map(|layer| match layer.as_slice() {
            [] => quote! {},
            [single] => {
                let acc = accessor(single);
                quote! { #acc.async_drop().await; }
            }
            many => {
                let futs = many.iter().map(|m| {
                    let acc = accessor(m);
                    quote! { #acc.async_drop() }
                });
                quote! {
                    #krate::__private::join_all(vec![
                        #( Box::pin(#futs) as ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ()> + Send>> ),*
                    ]).await;
                }
            }
        })
        .collect()
}

/// Synthesize `where Ty: eulogy::AsyncDrop` for each annotated entry, so
/// users with generic structs/enums don't need to spell the bound out
/// themselves.
fn push_where_bounds(where_clause: &mut WhereClause, entries: &[Entry], krate: &TokenStream2) {
    for entry in entries {
        let ty = &entry.ty;
        where_clause
            .predicates
            .push(parse_quote!(#ty: #krate::AsyncDrop));
    }
}

/// The identifier a field is bound to inside an enum match arm. Named fields
/// keep their own name (so struct-pattern shorthand works); positional
/// tuple fields get a synthetic name since `0`, `1`, ... aren't valid
/// identifiers.
fn binding_ident(member: &Member) -> Ident {
    match member {
        Member::Named(ident) => ident.clone(),
        Member::Unnamed(index) => format_ident!("__eulogy_field_{}", index.index),
    }
}

/// Build the match-arm pattern for one enum variant. Fields not present in
/// `entry_members` (i.e. `#[eulogy(skip)]` or unannotated) are ignored via
/// `..` for named fields, or bound to `_` for positional tuple fields (named
/// patterns match by name so a single trailing `..` suffices; tuple patterns
/// are positional, so every slot must be listed explicitly).
fn variant_pattern(
    enum_name: &Ident,
    variant: &Variant,
    field_list: &[(Member, &syn::Field)],
    entry_members: &[Member],
) -> TokenStream2 {
    let variant_ident = &variant.ident;
    let path = quote! { #enum_name::#variant_ident };

    match &variant.fields {
        Fields::Named(_) => {
            let bindings = field_list.iter().filter_map(|(m, _)| {
                if entry_members.contains(m) {
                    Some(quote! { #m })
                } else {
                    None
                }
            });
            quote! { #path { #(#bindings,)* .. } }
        }
        Fields::Unnamed(_) => {
            let positions = field_list.iter().map(|(m, _)| {
                if entry_members.contains(m) {
                    let ident = binding_ident(m);
                    quote! { #ident }
                } else {
                    quote! { _ }
                }
            });
            quote! { #path ( #(#positions,)* ) }
        }
        Fields::Unit => path,
    }
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let generics = &input.generics;
    let krate = eulogy_crate();

    let (impl_generics, ty_generics, existing_where) = generics.split_for_impl();
    let mut where_clause: WhereClause = match existing_where {
        Some(w) => w.clone(),
        None => parse_quote!(where),
    };

    let body = match &input.data {
        Data::Struct(data) => {
            let field_list = collect_field_list(&data.fields);
            let all_members: Vec<Member> = field_list.iter().map(|(m, _)| m.clone()).collect();
            let entries = parse_entries(&field_list)?;
            validate_after_refs(&entries, &all_members, "this struct")?;
            push_where_bounds(&mut where_clause, &entries, &krate);
            let layers = topo_layers(&entries)?;
            let calls = layer_calls(&layers, &krate, |m| quote! { self.#m });
            quote! { #(#calls)* }
        }
        Data::Enum(data) => {
            let mut arms: Vec<TokenStream2> = Vec::new();
            for variant in &data.variants {
                let field_list = collect_field_list(&variant.fields);
                let all_members: Vec<Member> = field_list.iter().map(|(m, _)| m.clone()).collect();
                let entries = parse_entries(&field_list)?;
                let container_desc = format!("variant `{}`", variant.ident);
                validate_after_refs(&entries, &all_members, &container_desc)?;
                push_where_bounds(&mut where_clause, &entries, &krate);
                let layers = topo_layers(&entries)?;

                let entry_members: Vec<Member> = entries.iter().map(|e| e.member.clone()).collect();
                let pattern = variant_pattern(name, variant, &field_list, &entry_members);
                let calls = layer_calls(&layers, &krate, |m| {
                    let ident = binding_ident(m);
                    quote! { #ident }
                });
                arms.push(quote! { #pattern => { #(#calls)* } });
            }
            quote! {
                match self {
                    #(#arms)*
                }
            }
        }
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "#[derive(AsyncDrop)] does not support unions — there is no safe way to \
                 know which field is active, so the derive cannot know what to drop",
            ));
        }
    };

    Ok(quote! {
        impl #impl_generics #krate::AsyncDrop for #name #ty_generics #where_clause {
            async fn async_drop(self) {
                #body
            }
        }
    })
}
