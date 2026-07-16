use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{quote, ToTokens};
use syn::spanned::Spanned;
use syn::{
    parse_macro_input, parse_quote, Data, DeriveInput, Expr, ExprArray, Fields, Ident, Index, Lit,
    Member, Meta, Type, WhereClause,
};

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
/// Works on structs with named fields, tuple structs, and unit structs. Every
/// field is dropped by default — all field types must implement `AsyncDrop`.
/// Opt out individual fields with `#[eulogy(skip)]`. Use
/// `#[eulogy(after = [field_a, field_b])]` to enforce ordering: a field with
/// `after` is dropped only once the listed fields finish. Tuple-struct fields
/// are referenced by positional index (`after = [0, 1]`).
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
///
/// #[derive(AsyncDrop)]
/// struct TupleResource(ChildDir, #[eulogy(after = [0])] ParentDir);
///
/// #[derive(AsyncDrop)]
/// struct Sentinel; // unit struct — async_drop is a no-op
/// ```
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

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let generics = &input.generics;

    // Collapse Named / Unnamed / Unit into a single (Member, &Field) list.
    let field_list: Vec<(Member, &syn::Field)> = match &input.data {
        Data::Struct(data) => match &data.fields {
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
        },
        Data::Enum(_) | Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "#[derive(AsyncDrop)] only supports structs",
            ));
        }
    };

    let all_members: Vec<Member> = field_list.iter().map(|(m, _)| m.clone()).collect();

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

    // Validate every `after` reference: it must name a field, that field must
    // itself be annotated (not skipped), and it must not be self-referential.
    for entry in &entries {
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
                    format!("no field `{}` in this struct", dep.to_token_stream()),
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

    let krate = eulogy_crate();
    let layers = topo_layers(&entries)?;

    // Each layer is dropped concurrently, but a layer waits for the previous
    // one to finish. Fields with no `after` deps end up in layer 0 together.
    let layer_calls: Vec<TokenStream2> = layers
        .iter()
        .map(|layer| match layer.as_slice() {
            [] => quote! {},
            [single] => quote! { self.#single.async_drop().await; },
            many => {
                let futs = many.iter().map(|m| quote! { self.#m.async_drop() });
                quote! {
                    #krate::__private::join_all(vec![
                        #( Box::pin(#futs) as ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ()> + Send>> ),*
                    ]).await;
                }
            }
        })
        .collect();

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
                #(#layer_calls)*
            }
        }
    })
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
