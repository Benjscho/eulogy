use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use proc_macro_crate::{crate_name, FoundCrate};
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Data, Fields, Ident, Meta, Expr, ExprArray};

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
/// Annotate fields with `#[eulogy]` to have their `async_drop()` called.
/// Use `#[eulogy(after = [field_a, field_b])]` to specify that a field
/// should be dropped only after the listed fields have completed their drop.
///
/// # Example
///
/// ```ignore
/// #[derive(AsyncDrop)]
/// struct MyResource {
///     #[eulogy]
///     child: ChildDir,
///     #[eulogy(after = [child])]
///     parent: ParentDir,
///     name: String, // normal drop
/// }
/// ```
///
/// Generates drop order: `child` first, then `parent`.
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
    after: Vec<Ident>,
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

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

    let mut entries: Vec<Entry> = Vec::new();

    for f in fields.iter() {
        let Some(attr) = f.attrs.iter().find(|a| a.path().is_ident("eulogy")) else {
            continue;
        };
        let ident = f.ident.clone().expect("named field");
        let mut after = Vec::new();

        if let Meta::List(_) = &attr.meta {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("after") {
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
                } else {
                    Err(syn::Error::new_spanned(
                        &meta.path,
                        "unknown #[eulogy(...)] key — expected `after`",
                    ))
                }
            })?;
        }

        entries.push(Entry { ident, after });
    }

    let sorted = topo_sort(&entries)?;

    let drop_calls: Vec<_> = sorted
        .iter()
        .map(|ident| quote! { self.#ident.async_drop().await; })
        .collect();

    let krate = eulogy_crate();
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
