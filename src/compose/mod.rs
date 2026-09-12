//! XQuery-style composition from Rust.
//!
//! This module lets a host build XML from query results without an XQuery
//! processor. The XPath 2.0 engine already evaluates every expression a
//! FLWOR body needs — `max`, `count`, `avg`, `distinct-values`, quantified
//! expressions, date component extraction; Rust supplies the iteration and
//! the binding; and this module supplies the composition: a place to keep the
//! documents, a description of the result elements, and the emission rules
//! that make the result an XDM tree rather than a string of tags.
//!
//! There is no query parser here, no FLWOR runtime, and no new value model.
//!
//! # The pieces
//!
//! | Piece | What it is |
//! |---|---|
//! | [`Composer`] | the arena, the name table, the namespace context, and a cache of compiled expressions |
//! | [`Doc`] | a finished document in that arena — loaded or built — as a `Copy` handle |
//! | [`Value`] | one evaluation's result: an XDM sequence, with the conversions a host asks for |
//! | [`IntoXPathValue`] | the way Rust values, navigators and sequences are bound to `$variables` |
//! | [`Form`] / [`Content`] / [`AttrValue`] / [`Name`] | a result element, described but not yet built |
//! | [`IntoContent`] | the way Rust values become element content |
//! | [`Emitter`] | the walk that turns forms into a document |
//! | [`order`] | `order by`, with XDM comparison and stable sorting |
//! | [`pipe`] | fallible iterator pipelines: the FLWOR clauses as adapters |
//! | [`ComposeError`] | every way a composition can fail |
//!
//! # Lifetimes
//!
//! A [`Composer<'a>`] borrows an arena and a name table. Every document it
//! loads or builds is finalized into that arena, so every navigator,
//! [`Value`] and [`Form`] shares the one lifetime `'a`: a node taken from one
//! document splices into another with no lifetime work at all. The price is
//! that nothing is freed until the composer and its arena go away, which
//! makes the model one composer per request or per batch.
//!
//! Every method takes `&self`, so a composer can be used inside iterator
//! closures and nested loops. It is neither `Send` nor `Sync`.
//!
//! # Values
//!
//! An evaluation hands back a [`Value`], which is the engine's own sequence
//! with nothing atomized, copied or flattened. `nodes`, `atomics`, `boolean`,
//! `string`, `number` and `key` each convert exactly as far as asked, and each
//! refuses rather than guessing: an atomic item where a node was required is
//! [`ComposeError::NotANode`], and several items where one was required is
//! [`ComposeError::NotSingleton`].
//!
//! # Forms
//!
//! A [`Form`] is an owned, eager tree holding no closures. Every Rust value
//! inside one has already been evaluated, which is why a `?` inside a form's
//! construction returns from the enclosing function and why a form can be
//! returned, stored and spliced later.
//!
//! # Emission
//!
//! [`Composer::build`] walks the form once. Prefixes resolve against the
//! forms' own declarations first and the composer's table second; the
//! declarations an element must carry are computed by the namespace fixup, so
//! every name in the result resolves; attribute sequences are atomized and
//! joined with single spaces; and spliced nodes are copied with the
//! constructor content rules — atomic values separated from each other by one
//! space and from a node by nothing, document nodes flattened, empty
//! sequences contributing nothing at all. An empty sequence spliced into an
//! element therefore leaves an empty element, which is the XDM answer and not
//! something the host has to special-case.
//!
//! # Pipelines, and what is not lazy
//!
//! [`pipe`] turns the FLWOR clauses into fallible iterator adapters: a source
//! supplies rows, `try_map` carries a `let` binding along in a tuple,
//! `try_filter` is `where`, `try_flat_map` is a nested `for`, and `order_by`
//! is `order by`. Errors travel to the consumer instead of turning into
//! missing rows, and a pipe is fused after the first one.
//!
//! The adapters are pulled on demand, but three things are eager and stay
//! eager: an expression that feeds a source has already been evaluated and
//! its sequence is already materialized; `order_by` buffers its input, which
//! is what makes it a stable sort with one key evaluation per row; and a
//! completed form tree is a tree. The pipelines remove the intermediate
//! vectors between stages and make the query's structure visible. They do not
//! promise XML streaming, incremental evaluation, or a form-free result.
//!
//! # Example
//!
//! One document in, one document out, with a filter, a sort and a projection:
//!
//! ```
//! use bumpalo::Bump;
//! use xsd_schema::compose::order::{Direction, EmptyOrder};
//! use xsd_schema::compose::{pipe, ComposeError, Composer, Content, Form, IntoContent, Name};
//! use xsd_schema::document::SerializeOptions;
//! use xsd_schema::namespace::NameTable;
//!
//! let arena = Bump::new();
//! let names = NameTable::new();
//! let c = Composer::new(&arena, &names);
//!
//! let stock = c.load_str(
//!     "<stock>\
//!        <part name='bolt' qty='7'/>\
//!        <part name='nut' qty='0'/>\
//!        <part name='washer' qty='3'/>\
//!      </stock>",
//! )?;
//!
//! // for $p in //part where $p/@qty > 0 order by $p/@name return <part>…</part>
//! let rows = pipe::nodes(c.eval("//part", &[], Some(stock.root()), Vec::new())?)
//!     .try_filter(|p| c.eval("@qty > 0", &[], Some(p.clone()), Vec::new())?.boolean())
//!     .order_by(Direction::Ascending, EmptyOrder::Least, |p| {
//!         c.eval("@name", &[], Some(p.clone()), Vec::new())?.key()
//!     })?
//!     .try_map(|p| {
//!         let mut part = Form::new(Name::local("part"));
//!         part.push(c.eval("string(@name)", &[], Some(p), Vec::new())?.into_content());
//!         Ok(part)
//!     });
//!
//! let mut result = Form::new(Name::local("in_stock"));
//! for row in rows {
//!     result.push(Content::Element(row?));
//! }
//!
//! assert_eq!(
//!     c.build(result)?.to_xml(&SerializeOptions::default())?,
//!     "<in_stock><part>bolt</part><part>washer</part></in_stock>",
//! );
//! # Ok::<(), ComposeError>(())
//! ```

pub mod composer;
pub mod emit;
pub mod error;
pub mod form;
pub mod order;
pub mod pipe;
pub mod value;

pub use composer::{Composer, Doc};
pub use emit::Emitter;
pub use error::ComposeError;
pub use form::{AttrValue, Content, Form, IntoContent, Name};
pub use value::{IntoContextNode, IntoXPathValue, Value};

/// The navigator every composed value and form uses.
///
/// v1 fixes the navigator type to the document buffer's own. A composer
/// generic over [`DomNavigator`](crate::navigator::DomNavigator) is a
/// mechanical extension, and is not needed until a host wants to splice nodes
/// from another tree implementation.
pub type Nav<'a> = crate::document::BufferDocNavigator<'a>;

#[cfg(test)]
mod use_case_tests {
    use std::path::{Path, PathBuf};

    use bumpalo::Bump;

    use super::order::{Direction, EmptyOrder};
    use super::{pipe, ComposeError, Composer, Content, Form, IntoContent, IntoXPathValue, Name};
    use crate::document::SerializeOptions;
    use crate::namespace::NameTable;

    /// The XQuery 1.0 test-suite root, or `None` when it is not around.
    ///
    /// Same convention as the conformance drivers: an environment variable
    /// first, then the checkout next to this crate, and a printed message
    /// rather than a failure when neither is there.
    fn suite_root() -> Option<PathBuf> {
        let candidate = match std::env::var_os("XQTS_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from("../../XQTS_1_0_2"),
        };
        if candidate.join("TestSources/items.xml").is_file() {
            Some(candidate)
        } else {
            println!(
                "skipping: no XQuery test suite at {} (set XQTS_DIR)",
                candidate.display()
            );
            None
        }
    }

    /// The highest bid for every bicycle, in item-number order.
    ///
    /// The query this rewrites, from the test suite's relational use case:
    ///
    /// ```xquery
    /// <result>{
    ///   for $i in $input-context1//item_tuple
    ///   let $b := $input-context2//bid_tuple[itemno = $i/itemno]
    ///   where contains($i/description, "Bicycle")
    ///   order by $i/itemno
    ///   return <item_tuple>{ $i/itemno }{ $i/description }
    ///          <high_bid>{ max($b/bid) }</high_bid></item_tuple>
    /// }</result>
    /// ```
    ///
    /// Written with [`Composer::eval`] and the [`Form`] API directly, since
    /// the macros are a later step.
    fn highest_bid_per_bicycle(root: &Path) -> Result<String, ComposeError> {
        let arena = Bump::new();
        let names = NameTable::new();
        let c = Composer::new(&arena, &names);

        let items = c.load_file(root.join("TestSources/items.xml"))?;
        let bids = c.load_file(root.join("TestSources/bids.xml"))?;

        // for $i in //item_tuple
        let rows = pipe::nodes(c.eval("//item_tuple", &[], Some(items.root()), Vec::new())?)
            // let $b := //bid_tuple[itemno = $i/itemno]
            .try_map(|i| {
                let b = c.eval(
                    "//bid_tuple[itemno = $i/itemno]",
                    &["i"],
                    Some(bids.root()),
                    vec![("i", (&i).into_xpath_value())],
                )?;
                Ok((i, b))
            })
            // where contains($i/description, "Bicycle")
            .try_filter(|(i, _)| {
                c.eval(
                    "contains(description, 'Bicycle')",
                    &[],
                    Some(i.clone()),
                    Vec::new(),
                )?
                .boolean()
            })
            // order by $i/itemno
            .order_by(Direction::Ascending, EmptyOrder::Least, |(i, _)| {
                c.eval("itemno", &[], Some(i.clone()), Vec::new())?.key()
            })?
            // return <item_tuple>…</item_tuple>
            .try_map(|(i, b)| {
                let mut high_bid = Form::new(Name::local("high_bid"));
                high_bid.push(
                    c.eval(
                        "max($b/bid)",
                        &["b"],
                        None,
                        vec![("b", (&b).into_xpath_value())],
                    )?
                    .into_content(),
                );

                let mut item = Form::new(Name::local("item_tuple"));
                item.push(
                    c.eval("itemno", &[], Some(i.clone()), Vec::new())?
                        .into_content(),
                )
                .push(
                    c.eval("description", &[], Some(i.clone()), Vec::new())?
                        .into_content(),
                )
                .push(Content::Element(high_bid));
                Ok(item)
            });

        let mut result = Form::new(Name::local("result"));
        for row in rows {
            result.push(Content::Element(row?));
        }

        c.build(result)?.to_xml(&SerializeOptions::default())
    }

    #[test]
    fn the_relational_use_case_matches_its_oracle() {
        let Some(root) = suite_root() else { return };

        let written = highest_bid_per_bicycle(&root).expect("the composition succeeds");

        // The expected result of the use case, byte for byte.
        assert_eq!(
            written,
            "<result><item_tuple><itemno>1001</itemno><description>Red Bicycle</description><high_bid>55</high_bid></item_tuple>\
             <item_tuple><itemno>1003</itemno><description>Old Bicycle</description><high_bid>20</high_bid></item_tuple>\
             <item_tuple><itemno>1007</itemno><description>Racing Bicycle</description><high_bid>225</high_bid></item_tuple>\
             <item_tuple><itemno>1008</itemno><description>Broken Bicycle</description><high_bid/></item_tuple></result>",
        );
    }
}
