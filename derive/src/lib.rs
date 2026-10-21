//! `#[derive(Entity)]` for agent_wiki: a struct whose records the index
//! keeps in a table of their own.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitInt, LitStr, parse_macro_input};

/// Implements `agent_wiki::Entity` for a struct with named fields.
///
/// On the struct: `#[entity(name = "…", version = N)]`, both optional. The
/// name defaults to the struct's name in snake case; the version to 0.
///
/// On a field: `#[entity(index)]` for a b-tree index, `#[entity(fulltext)]`
/// for the type's full-text index. Field types: `String`, integers, `f32`,
/// `f64`, `bool`, and `Option` of those.
#[proc_macro_derive(Entity, attributes(entity))]
pub fn derive_entity(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let ident = &input.ident;
    let mut name = snake_case(&ident.to_string());
    let mut version = 0u32;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("entity")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                name = meta.value()?.parse::<LitStr>()?.value();
                Ok(())
            } else if meta.path.is_ident("version") {
                version = meta.value()?.parse::<LitInt>()?.base10_parse()?;
                Ok(())
            } else {
                Err(meta.error("expected `name` or `version`"))
            }
        })?;
    }
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(ident, "Entity needs a struct"));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(ident, "Entity needs named fields"));
    };
    let mut fields = Vec::new();
    let mut values = Vec::new();
    for field in &named.named {
        let field_ident = field.ident.as_ref().expect("named field");
        let field_name = field_ident.to_string();
        let field_name = field_name.strip_prefix("r#").unwrap_or(&field_name);
        let ty = &field.ty;
        let (mut index, mut fulltext) = (false, false);
        for attr in field.attrs.iter().filter(|a| a.path().is_ident("entity")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("index") {
                    index = true;
                    Ok(())
                } else if meta.path.is_ident("fulltext") {
                    fulltext = true;
                    Ok(())
                } else {
                    Err(meta.error("expected `index` or `fulltext`"))
                }
            })?;
        }
        if matches!(field_name, "id" | "page" | "ordinal") {
            return Err(syn::Error::new_spanned(
                field_ident,
                "`id`, `page` and `ordinal` are the index's own columns",
            ));
        }
        fields.push(quote! {
            ::agent_wiki::Field {
                name: #field_name,
                kind: <#ty as ::agent_wiki::EntityField>::KIND,
                optional: <#ty as ::agent_wiki::EntityField>::OPTIONAL,
                index: #index,
                fulltext: #fulltext,
            }
        });
        values.push(quote! {
            ::agent_wiki::EntityField::value(&self.#field_ident)
        });
    }
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics ::agent_wiki::Entity for #ident #ty_generics #where_clause {
            const NAME: &'static str = #name;
            const VERSION: u32 = #version;

            fn fields() -> ::std::vec::Vec<::agent_wiki::Field> {
                ::std::vec![#(#fields),*]
            }

            fn values(&self) -> ::std::vec::Vec<::agent_wiki::FieldValue> {
                ::std::vec![#(#values),*]
            }
        }
    })
}

/// `TicketRef` → `ticket_ref`.
fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}
