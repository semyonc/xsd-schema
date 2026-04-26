//! XPath 2.0 special/context functions.
//!
//! This module implements:
//! - fn:position() - return context position
//! - fn:last() - return context size
//! - fn:trace($value, $label?) - debug output and passthrough
//! - fn:data($arg) - atomize a sequence
//! - fn:default-collation() - return default collation URI

use crate::xpath::context::DynamicContext;
use crate::xpath::error::XPathError;
use crate::xpath::DomNavigator;

use super::{atomize_sequence, XPathValue};
use crate::xpath::iterator::XmlItem;

/// Default collation URI (codepoint collation)
const DEFAULT_COLLATION: &str = "http://www.w3.org/2005/xpath-functions/collation/codepoint";

/// fn:position() as xs:integer
///
/// Returns the context position of the current item within the sequence
/// being processed. This is a 1-based position.
///
/// Raises XPDY0002 if the context item is undefined.
pub fn position<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if !args.is_empty() {
        return Err(XPathError::wrong_number_of_arguments(
            "position",
            0,
            args.len(),
        ));
    }
    // Context must be defined for position()
    if context.context_item.is_none() && context.context_position == 0 {
        return Err(XPathError::XPDY0002 {
            message: "Context is undefined for fn:position()".to_string(),
        });
    }
    Ok(XPathValue::integer(context.context_position as i64))
}

/// fn:last() as xs:integer
///
/// Returns the context size (total number of items in the sequence
/// being processed).
///
/// Raises XPDY0002 if the context item is undefined.
pub fn last<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if !args.is_empty() {
        return Err(XPathError::wrong_number_of_arguments("last", 0, args.len()));
    }
    // Context must be defined for last()
    if context.context_item.is_none() && context.context_size == 0 {
        return Err(XPathError::XPDY0002 {
            message: "Context is undefined for fn:last()".to_string(),
        });
    }
    Ok(XPathValue::integer(context.context_size as i64))
}

/// fn:trace($value as item()*, $label as xs:string?) as item()*
///
/// Returns $value unchanged, after writing $label and $value to trace output.
/// This function is intended for debugging.
///
/// XPath 2.0 requires two arguments, but we support 1-2 for flexibility.
pub fn trace<N: DomNavigator>(
    context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.is_empty() || args.len() > 2 {
        return Err(XPathError::wrong_number_of_arguments(
            "trace",
            2,
            args.len(),
        ));
    }

    // Get optional label (second argument)
    let label = if args.len() == 2 {
        let label_arg = args.pop().unwrap();
        super::atomize_to_string_opt(label_arg)?
    } else {
        None
    };

    // Get value (first argument) - we'll return this unchanged
    let value = args.remove(0);

    // Write trace output to stderr only when trace is enabled
    if context.static_context.trace_enabled {
        let value_str = value_to_trace_string(&value);
        if let Some(label) = label {
            eprintln!("[trace] {}: {}", label, value_str);
        } else {
            eprintln!("[trace] {}", value_str);
        }
    }

    // Return value unchanged
    Ok(value)
}

/// Convert an XPathValue to a string for trace output.
fn value_to_trace_string<N: DomNavigator>(value: &XPathValue<N>) -> String {
    match value {
        XPathValue::Empty => "()".to_string(),
        XPathValue::Item(item) => item_to_trace_string(item),
        XPathValue::Sequence(items) => {
            let strs: Vec<String> = items.iter().map(item_to_trace_string).collect();
            format!("({})", strs.join(", "))
        }
    }
}

/// Convert an XmlItem to a string for trace output.
fn item_to_trace_string<N: DomNavigator>(item: &XmlItem<N>) -> String {
    match item {
        XmlItem::Atomic(value) => value.to_string_value(),
        XmlItem::Node(nav) => format!("<{}...>", nav.name()),
    }
}

/// fn:data($arg as item()*) as xs:anyAtomicType*
///
/// Returns the atomized value of each item in $arg.
///
/// For atomic values, returns the value itself.
/// For nodes, returns the typed value of the node.
pub fn data<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    mut args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if args.len() != 1 {
        return Err(XPathError::wrong_number_of_arguments("data", 1, args.len()));
    }

    let arg = args.remove(0);

    // Atomize the entire sequence
    let atomized = atomize_sequence(arg)?;

    // Convert back to XPathValue
    let items: Vec<XmlItem<N>> = atomized.into_iter().map(XmlItem::Atomic).collect();

    Ok(XPathValue::from_sequence(items))
}

/// fn:default-collation() as xs:string
///
/// Returns the value of the default collation property from the static context.
/// The default collation is the Unicode codepoint collation.
pub fn default_collation<N: DomNavigator>(
    _context: &mut DynamicContext<'_, N>,
    args: Vec<XPathValue<N>>,
) -> Result<XPathValue<N>, XPathError> {
    if !args.is_empty() {
        return Err(XPathError::wrong_number_of_arguments(
            "default-collation",
            0,
            args.len(),
        ));
    }
    Ok(XPathValue::string(DEFAULT_COLLATION))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::table::NameTable;
    use crate::types::value::XmlValue;
    use crate::xpath::context::XPathContext;
    use crate::xpath::RoXmlNavigator;

    fn create_context<'a>(names: &'a NameTable) -> DynamicContext<'a, RoXmlNavigator<'a>> {
        let static_ctx = XPathContext::new(names);
        // Use Box::leak for tests only to get 'a lifetime
        let static_ctx = Box::leak(Box::new(static_ctx));
        DynamicContext::new(static_ctx, 0).with_position(3, 10)
    }

    #[test]
    fn test_position() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        ctx.context_position = 5;

        let result = position(&mut ctx, vec![]).unwrap();
        if let XPathValue::Item(XmlItem::Atomic(value)) = result {
            assert_eq!(
                value.as_integer().map(|i| i.to_string()),
                Some("5".to_string())
            );
        } else {
            panic!("Expected integer");
        }
    }

    #[test]
    fn test_last() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);
        ctx.context_size = 10;

        let result = last(&mut ctx, vec![]).unwrap();
        if let XPathValue::Item(XmlItem::Atomic(value)) = result {
            assert_eq!(
                value.as_integer().map(|i| i.to_string()),
                Some("10".to_string())
            );
        } else {
            panic!("Expected integer");
        }
    }

    #[test]
    fn test_default_collation() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = default_collation(&mut ctx, vec![]).unwrap();
        if let XPathValue::Item(XmlItem::Atomic(value)) = result {
            assert_eq!(value.as_string(), Some(DEFAULT_COLLATION));
        } else {
            panic!("Expected string");
        }
    }

    #[test]
    fn test_data_atomic() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let input = XPathValue::string("hello");
        let result = data(&mut ctx, vec![input]).unwrap();

        if let XPathValue::Item(XmlItem::Atomic(value)) = result {
            assert_eq!(value.as_string(), Some("hello"));
        } else {
            panic!("Expected atomic value");
        }
    }

    #[test]
    fn test_data_sequence() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let items = vec![
            XmlItem::Atomic(XmlValue::integer(1.into())),
            XmlItem::Atomic(XmlValue::integer(2.into())),
            XmlItem::Atomic(XmlValue::integer(3.into())),
        ];
        let input = XPathValue::Sequence(items);
        let result = data(&mut ctx, vec![input]).unwrap();

        match result {
            XPathValue::Sequence(items) => {
                assert_eq!(items.len(), 3);
            }
            _ => panic!("Expected sequence"),
        }
    }

    #[test]
    fn test_trace_passthrough() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let input = XPathValue::string("test value");
        let label = XPathValue::string("debug");
        let result = trace(&mut ctx, vec![input, label]).unwrap();

        // trace returns the input unchanged
        if let XPathValue::Item(XmlItem::Atomic(value)) = result {
            assert_eq!(value.as_string(), Some("test value"));
        } else {
            panic!("Expected string");
        }
    }

    #[test]
    fn test_position_wrong_arity() {
        let names = NameTable::new();
        let mut ctx = create_context(&names);

        let result = position(&mut ctx, vec![XPathValue::string("extra")]);
        assert!(result.is_err());
    }
}
