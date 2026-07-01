use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use proc_macro_crate::{crate_name, FoundCrate};
use quote::quote;
use syn::spanned::Spanned;
use syn::{parse_macro_input, parse_quote, DeriveInput, Data, Fields, Ident, Meta, Expr, ExprArray, Type, WhereClause};

/// Resolve the path to the `eulogy` crate, honoring any rename via
/// `[dependencies] foo = { package = "eulogy" }`.
fn eulogy_crate() -> TokenStream2 {
    match crate_name("eulogy") {
        Ok(FoundCrate::Itself) => quote!(crate),
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name, Span::call_site());
            quote!(::#ident)
        }
        // Fallback to the canonical name; will emit a normal "unresolved
        // crate `eulogy`" error if the user really hasn't depended on it.
        Err(_) => quote!(::eulogy),
    }
}

/// Derive `AsyncDrop` for a struct.
///
/// Every field is dropped by default — all field types must implement
/// `AsyncDrop`. Opt out individual fields with `#[eulogy(skip)]`.
/// Use `#[eulogy(after = [field_a, field_b])]` to enforce ordering: a
/// field with `after` is dropped only once the listed fields finish.
///
/// # Example
///
/// ```ignore
/// #[derive(AsyncDrop)]
/// struct MyResource {
///     child: ChildDir,
///     #[eulogy(after = [child])]
///     parent: ParentDir,
///     #[eulogy(skip)]
///     name: String, // sync drop
/// }
/// ```
///
/// Generates drop order: `child` first, then `parent`. `name` is dropped
/// normally (synchronously) when the struct goes out of scope.
#[proc_macro_derive(AsyncDrop, attributes(eulogy))]
pub fn derive_async_drop(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

struct Entry {
    ident: Ident,
    ty: Type,
    after: Vec<Ident>,
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let generics = &input.generics;

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    &input.ident,
                    "#[derive(AsyncDrop)] does not support tuple structs — use a struct with named fields",
                ));
            }
            Fields::Unit => {
                return Err(syn::Error::new_spanned(
                    &input.ident,
                    "#[derive(AsyncDrop)] does not support unit structs — there are no fields to drop",
                ));
            }
        },
        Data::Enum(_) | Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "#[derive(AsyncDrop)] only supports structs",
            ));
        }
    };

    let all_field_names: Vec<Ident> = fields
        .iter()
        .filter_map(|f| f.ident.clone())
        .collect();

    let mut entries: Vec<Entry> = Vec::new();

    for f in fields.iter() {
        let ident = f.ident.clone().expect("named field");
        let ty = f.ty.clone();
        let attr = f.attrs.iter().find(|a| a.path().is_ident("eulogy"));

        let mut after = Vec::new();
        let mut skip = false;
        let mut skip_span: Option<proc_macro2::Span> = None;
        let mut after_span: Option<proc_macro2::Span> = None;

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
                                        after.push(seg.ident.clone());
                                    } else {
                                        return Err(syn::Error::new_spanned(
                                            elem,
                                            "expected a field name",
                                        ));
                                    }
                                }
                                other => {
                                    return Err(syn::Error::new_spanned(
                                        other,
                                        "expected a field name, not an expression",
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

        entries.push(Entry { ident, ty, after });
    }

    // Validate every `after` reference: it must name a field, that field must
    // itself be annotated with #[eulogy], and it must not be self-referential.
    for entry in &entries {
        for dep in &entry.after {
            if dep == &entry.ident {
                return Err(syn::Error::new_spanned(
                    dep,
                    format!("`{}` cannot list itself in #[eulogy(after = [...])]", dep),
                ));
            }
            if !all_field_names.iter().any(|f| f == dep) {
                return Err(syn::Error::new_spanned(
                    dep,
                    format!("no field named `{}` in this struct", dep),
                ));
            }
            if !entries.iter().any(|e| &e.ident == dep) {
                return Err(syn::Error::new_spanned(
                    dep,
                    format!(
                        "field `{}` is not annotated with #[eulogy] — it cannot appear in an `after` list",
                        dep
                    ),
                ));
            }
        }
    }

    let sorted = topo_sort(&entries)?;

    let drop_calls: Vec<_> = sorted
        .iter()
        .map(|ident| quote! { self.#ident.async_drop().await; })
        .collect();

    let krate = eulogy_crate();

    // Synthesize `where Ty: eulogy::AsyncDrop` for each annotated field so users
    // with generic structs (`struct Wrapper<T> { #[eulogy] inner: T }`) don't
    // need to spell the bound out themselves.
    let (impl_generics, ty_generics, existing_where) = generics.split_for_impl();
    let mut where_clause: WhereClause = match existing_where {
        Some(w) => w.clone(),
        None => parse_quote!(where),
    };
    for entry in &entries {
        let ty = &entry.ty;
        where_clause
            .predicates
            .push(parse_quote!(#ty: #krate::AsyncDrop));
    }

    Ok(quote! {
        impl #impl_generics #krate::AsyncDrop for #name #ty_generics #where_clause {
            async fn async_drop(self) {
                #(#drop_calls)*
            }
        }
    })
}

/// Topological sort: fields with no deps come first, fields with `after` come later.
fn topo_sort(entries: &[Entry]) -> syn::Result<Vec<Ident>> {
    let n = entries.len();
    let mut visited = vec![false; n];
    let mut in_stack = vec![false; n];
    let mut order = Vec::with_capacity(n);

    for i in 0..n {
        visit(i, entries, &mut visited, &mut in_stack, &mut order)?;
    }

    Ok(order)
}

fn visit(
    idx: usize,
    entries: &[Entry],
    visited: &mut [bool],
    in_stack: &mut [bool],
    order: &mut Vec<Ident>,
) -> syn::Result<()> {
    if visited[idx] {
        return Ok(());
    }
    if in_stack[idx] {
        return Err(syn::Error::new_spanned(
            &entries[idx].ident,
            "cycle detected in #[eulogy(after = [...])] dependencies",
        ));
    }
    in_stack[idx] = true;

    for dep in &entries[idx].after {
        if let Some(dep_idx) = entries.iter().position(|e| &e.ident == dep) {
            visit(dep_idx, entries, visited, in_stack, order)?;
        }
    }

    in_stack[idx] = false;
    visited[idx] = true;
    order.push(entries[idx].ident.clone());
    Ok(())
}
