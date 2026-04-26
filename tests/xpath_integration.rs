#![cfg(feature = "xsd11")]
//! XPath integration tests using sample XML files.

use std::fs;
use std::path::PathBuf;
use xsd_schema::namespace::table::NameTable;
use xsd_schema::xpath::api::XPathExpr;
use xsd_schema::xpath::{DomNavigator, RoXmlNavigator, XPathContext};

fn get_examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")
}

/// Helper to extract string value from an XmlItem (node or atomic)
fn item_to_string<N: DomNavigator>(item: &xsd_schema::xpath::XmlItem<N>) -> String {
    match item {
        xsd_schema::xpath::XmlItem::Node(nav) => nav.value(),
        xsd_schema::xpath::XmlItem::Atomic(val) => val.to_string_value(),
    }
}

/// Test: //book[price > 35]/title on books.xml
/// Expected: "The First Book" (price 44.95)
#[test]
fn test_book_price_filter() {
    let xml_path = get_examples_dir().join("books.xml");
    let xml_content = fs::read_to_string(&xml_path).expect("Failed to read books.xml");

    let names = NameTable::new();
    let ctx = XPathContext::new(&names);

    let expr = XPathExpr::compile("//book[price > 35]/title", &ctx)
        .expect("Failed to compile XPath expression");

    let doc = roxmltree::Document::parse(&xml_content).expect("Failed to parse XML");
    let nav = RoXmlNavigator::new(&doc);

    let result = expr
        .evaluator(&ctx)
        .run_with_node(nav)
        .expect("Failed to evaluate XPath");

    // Should find one book with price > 35 (The First Book with price 44.95)
    assert_eq!(result.len(), 1, "Expected exactly one book with price > 35");

    // Get the string value using run_string
    let title = expr
        .evaluator(&ctx)
        .run_with_node(RoXmlNavigator::new(&doc))
        .expect("Failed to evaluate XPath");

    let items = title.into_vec();
    let title_str = item_to_string(&items[0]);

    assert_eq!(
        title_str, "The First Book",
        "Expected 'The First Book' as the title"
    );
}

/// Test: for $x in //item return concat($x/@partNum, '-', $x/productName) on purchaseOrder_utf8.xml
/// Expected: ["872-AA-Lawnmower", "926-AA-Baby Monitor"]
#[test]
fn test_flwor_item_concat() {
    let xml_path = get_examples_dir().join("purchaseOrder.xml");
    let xml_content = fs::read_to_string(&xml_path).expect("Failed to read purchaseOrder.xml");

    let names = NameTable::new();
    let ctx = XPathContext::new(&names);

    let expr = XPathExpr::compile(
        "for $x in //item return concat($x/@partNum, '-', $x/productName)",
        &ctx,
    )
    .expect("Failed to compile XPath expression");

    let doc = roxmltree::Document::parse(&xml_content).expect("Failed to parse XML");
    let nav = RoXmlNavigator::new(&doc);

    let result = expr
        .evaluator(&ctx)
        .run_with_node(nav)
        .expect("Failed to evaluate XPath");

    // Should return a sequence of two concatenated strings
    assert_eq!(result.len(), 2, "Expected two items in the result");

    let items: Vec<String> = result.into_vec().iter().map(item_to_string).collect();

    assert_eq!(
        items[0], "872-AA-Lawnmower",
        "First item should be 872-AA-Lawnmower"
    );
    assert_eq!(
        items[1], "926-AA-Baby Monitor",
        "Second item should be 926-AA-Baby Monitor"
    );
}

/// Test: //title[1] on books.xml
/// Expected: "The First Book" (first title in document order)
///
/// Note: In XPath 2.0, `//title[1]` is equivalent to `(//title)[1]` and returns
/// only the first title element in document order.
#[test]
fn test_positional_predicate() {
    let xml_path = get_examples_dir().join("books.xml");
    let xml_content = fs::read_to_string(&xml_path).expect("Failed to read books.xml");

    let names = NameTable::new();
    let ctx = XPathContext::new(&names);

    let expr = XPathExpr::compile("//title[1]", &ctx).expect("Failed to compile XPath expression");

    let doc = roxmltree::Document::parse(&xml_content).expect("Failed to parse XML");
    let nav = RoXmlNavigator::new(&doc);

    let result = expr
        .evaluator(&ctx)
        .run_with_node(nav)
        .expect("Failed to evaluate XPath");

    // //title[1] in XPath 2.0 returns only the first title element in document order
    assert_eq!(
        result.len(),
        1,
        "Expected one title element (the first in document order)"
    );

    // The result should be "The First Book"
    let items = result.into_vec();
    let first_title = item_to_string(&items[0]);
    assert_eq!(
        first_title, "The First Book",
        "First title in document order should be 'The First Book'"
    );
}

/// Test: //title[last()] on books.xml
/// Expected: "The Poet's First Poem" (last title in document order)
#[test]
fn test_last_predicate() {
    let xml_path = get_examples_dir().join("books.xml");
    let xml_content = fs::read_to_string(&xml_path).expect("Failed to read books.xml");

    let names = NameTable::new();
    let ctx = XPathContext::new(&names);

    let expr =
        XPathExpr::compile("//title[last()]", &ctx).expect("Failed to compile XPath expression");

    let doc = roxmltree::Document::parse(&xml_content).expect("Failed to parse XML");
    let nav = RoXmlNavigator::new(&doc);

    let result = expr
        .evaluator(&ctx)
        .run_with_node(nav)
        .expect("Failed to evaluate XPath");

    // //title[last()] returns only the last title element in document order
    assert_eq!(
        result.len(),
        1,
        "Expected one title element (the last in document order)"
    );

    // The result should be "The Poet's First Poem" (third book)
    let items = result.into_vec();
    let last_title = item_to_string(&items[0]);
    assert_eq!(
        last_title, "The Poet's First Poem",
        "Last title in document order should be 'The Poet's First Poem'"
    );
}
