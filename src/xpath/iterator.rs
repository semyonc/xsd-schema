//! XPath2 item and node iterator abstractions.
//!
//! Mirrors the design in `XML_NODE_ITERATOR_DESIGN.md`.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use num_bigint::BigInt;

use crate::types::XmlValue;

use super::DomNavigator;

/// XPath item (node or atomic value).
#[derive(Debug, Clone)]
pub enum XmlItem<N: DomNavigator> {
    Node(N),
    Atomic(XmlValue),
}

/// Borrowed view of an XPath item.
#[derive(Debug, Clone, Copy)]
pub enum XmlItemRef<'a, N: DomNavigator> {
    Node(&'a N),
    Atomic(&'a XmlValue),
}

impl<'a, N: DomNavigator> XmlItemRef<'a, N> {
    pub fn is_node(&self) -> bool {
        matches!(self, XmlItemRef::Node(_))
    }

    pub fn from_item(item: &'a XmlItem<N>) -> Self {
        match item {
            XmlItem::Node(node) => XmlItemRef::Node(node),
            XmlItem::Atomic(value) => XmlItemRef::Atomic(value),
        }
    }
}

/// Iterator over XPath items (nodes + atomic values).
///
/// This mirrors the .NET XPathNodeIterator shape: a cloneable cursor with
/// `current` and `move_next` semantics.
pub trait XmlNodeIterator: Clone {
    type Navigator: DomNavigator;

    /// Current item (None before first move_next or after end).
    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>>;

    /// 0-based position of the current item, or None if not started/finished.
    fn current_position(&self) -> Option<usize>;

    /// Advance to next item; returns false at end of sequence.
    fn move_next(&mut self) -> bool;

    /// 1-based sequential position for axis iteration.
    fn sequential_position(&self) -> Option<usize> {
        self.current_position().map(|pos| pos + 1)
    }

    /// Reset sequential position tracking (used by position filters).
    fn reset_sequential_position(&mut self) {}
}

fn clone_item_ref<N: DomNavigator>(item: XmlItemRef<'_, N>) -> XmlItem<N> {
    match item {
        XmlItemRef::Node(node) => XmlItem::Node(node.clone()),
        XmlItemRef::Atomic(value) => XmlItem::Atomic(value.clone()),
    }
}

/// Vector-backed iterator for simple tests and adapters.
#[derive(Debug, Clone)]
pub struct VecNodeIterator<N: DomNavigator> {
    items: Vec<XmlItem<N>>,
    index: Option<usize>,
}

impl<N: DomNavigator> VecNodeIterator<N> {
    pub fn new(items: Vec<XmlItem<N>>) -> Self {
        Self {
            items,
            index: None,
        }
    }
}

impl<N: DomNavigator> XmlNodeIterator for VecNodeIterator<N> {
    type Navigator = N;

    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>> {
        self.index.and_then(|i| match self.items.get(i) {
            Some(XmlItem::Node(node)) => Some(XmlItemRef::Node(node)),
            Some(XmlItem::Atomic(value)) => Some(XmlItemRef::Atomic(value)),
            None => None,
        })
    }

    fn current_position(&self) -> Option<usize> {
        self.index
    }

    fn move_next(&mut self) -> bool {
        let next = match self.index {
            None => 0,
            Some(i) => i + 1,
        };

        if next < self.items.len() {
            self.index = Some(next);
            true
        } else {
            self.index = None;
            false
        }
    }
}

/// Iterator that yields no items.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmptyIterator<N: DomNavigator> {
    _marker: PhantomData<N>,
}

impl<N: DomNavigator> EmptyIterator<N> {
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<N: DomNavigator> XmlNodeIterator for EmptyIterator<N> {
    type Navigator = N;

    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>> {
        None
    }

    fn current_position(&self) -> Option<usize> {
        None
    }

    fn move_next(&mut self) -> bool {
        false
    }
}

struct BufferedState<I: XmlNodeIterator> {
    source: I,
    buffer: Vec<XmlItem<I::Navigator>>,
    exhausted: bool,
}

/// Buffered iterator that can be replayed without re-reading the source.
#[derive(Clone)]
pub struct BufferedNodeIterator<I: XmlNodeIterator> {
    state: Rc<RefCell<BufferedState<I>>>,
    index: Option<usize>,
    current: Option<XmlItem<I::Navigator>>,
}

impl<I: XmlNodeIterator> BufferedNodeIterator<I> {
    pub fn new(source: I) -> Self {
        Self {
            state: Rc::new(RefCell::new(BufferedState {
                source,
                buffer: Vec::new(),
                exhausted: false,
            })),
            index: None,
            current: None,
        }
    }

    pub fn from_ref(source: &I) -> Self {
        Self::new(source.clone())
    }

    pub fn preload(source: I) -> Self {
        let mut iter = Self::new(source);
        iter.fill();
        iter
    }

    pub fn fill(&mut self) {
        let mut state = self.state.borrow_mut();
        if state.exhausted {
            return;
        }
        while state.source.move_next() {
            let next_item = state.source.current().map(clone_item_ref);
            if let Some(item) = next_item {
                state.buffer.push(item);
            } else {
                state.exhausted = true;
                return;
            }
        }
        state.exhausted = true;
    }
}

impl<I: XmlNodeIterator> XmlNodeIterator for BufferedNodeIterator<I> {
    type Navigator = I::Navigator;

    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>> {
        self.current.as_ref().map(XmlItemRef::from_item)
    }

    fn current_position(&self) -> Option<usize> {
        self.index
    }

    fn move_next(&mut self) -> bool {
        let next_index = match self.index {
            None => 0,
            Some(i) => i + 1,
        };

        let mut state = self.state.borrow_mut();
        if next_index < state.buffer.len() {
            self.index = Some(next_index);
            self.current = state.buffer.get(next_index).cloned();
            return true;
        }

        if state.exhausted {
            self.index = None;
            self.current = None;
            return false;
        }

        if state.source.move_next() {
            let next_item = state.source.current().map(clone_item_ref);
            if let Some(item) = next_item {
                state.buffer.push(item.clone());
                self.index = Some(next_index);
                self.current = Some(item);
                return true;
            }
            state.exhausted = true;
            self.index = None;
            self.current = None;
            return false;
        }

        state.exhausted = true;
        self.index = None;
        self.current = None;
        false
    }
}

/// Iterator over an inclusive integer range (XPath `to` expression).
#[derive(Debug, Clone)]
pub struct RangeIterator<N: DomNavigator> {
    min: BigInt,
    max: BigInt,
    current_value: Option<BigInt>,
    current_item: Option<XmlItem<N>>,
    index: Option<usize>,
    done: bool,
    _marker: PhantomData<N>,
}

impl<N: DomNavigator> RangeIterator<N> {
    pub fn new(min: BigInt, max: BigInt) -> Self {
        let done = min > max;
        Self {
            min,
            max,
            current_value: None,
            current_item: None,
            index: None,
            done,
            _marker: PhantomData,
        }
    }

    pub fn from_i64(min: i64, max: i64) -> Self {
        Self::new(BigInt::from(min), BigInt::from(max))
    }
}

impl<N: DomNavigator> XmlNodeIterator for RangeIterator<N> {
    type Navigator = N;

    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>> {
        self.current_item.as_ref().map(XmlItemRef::from_item)
    }

    fn current_position(&self) -> Option<usize> {
        self.index
    }

    fn move_next(&mut self) -> bool {
        if self.done {
            self.current_value = None;
            self.current_item = None;
            self.index = None;
            return false;
        }

        let next_value = match &self.current_value {
            None => self.min.clone(),
            Some(value) => value + 1,
        };

        if next_value > self.max {
            self.done = true;
            self.current_value = None;
            self.current_item = None;
            self.index = None;
            return false;
        }

        self.current_value = Some(next_value.clone());
        self.current_item = Some(XmlItem::Atomic(XmlValue::integer(next_value)));
        self.index = Some(match self.index {
            None => 0,
            Some(i) => i + 1,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::xpath::roxmltree::RoXmlNavigator;

    fn current_integer<N: DomNavigator>(iter: &impl XmlNodeIterator<Navigator = N>) -> BigInt {
        match iter.current() {
            Some(XmlItemRef::Atomic(value)) => value
                .as_integer()
                .expect("integer value")
                .clone(),
            _ => panic!("expected integer value"),
        }
    }

    #[test]
    fn test_empty_iterator() {
        let mut iter: EmptyIterator<RoXmlNavigator<'static>> = EmptyIterator::new();
        assert!(!iter.move_next());
        assert!(iter.current().is_none());
        assert!(iter.current_position().is_none());
    }

    #[test]
    fn test_range_iterator_values() {
        let mut iter: RangeIterator<RoXmlNavigator<'static>> = RangeIterator::from_i64(1, 3);
        assert!(iter.move_next());
        assert_eq!(current_integer(&iter), BigInt::from(1));
        assert_eq!(iter.current_position(), Some(0));
        assert_eq!(iter.sequential_position(), Some(1));

        assert!(iter.move_next());
        assert_eq!(current_integer(&iter), BigInt::from(2));
        assert_eq!(iter.current_position(), Some(1));
        assert_eq!(iter.sequential_position(), Some(2));

        assert!(iter.move_next());
        assert_eq!(current_integer(&iter), BigInt::from(3));
        assert_eq!(iter.current_position(), Some(2));
        assert_eq!(iter.sequential_position(), Some(3));

        assert!(!iter.move_next());
        assert!(iter.current().is_none());
    }

    #[test]
    fn test_range_iterator_empty() {
        let mut iter: RangeIterator<RoXmlNavigator<'static>> = RangeIterator::from_i64(5, 3);
        assert!(!iter.move_next());
        assert!(iter.current().is_none());
    }

    #[test]
    fn test_buffered_iterator_replays() {
        let source: VecNodeIterator<RoXmlNavigator<'static>> = VecNodeIterator::new(vec![
            XmlItem::Atomic(XmlValue::integer(BigInt::from(1))),
            XmlItem::Atomic(XmlValue::integer(BigInt::from(2))),
        ]);

        let mut buffered = BufferedNodeIterator::new(source);
        assert!(buffered.move_next());
        assert_eq!(current_integer(&buffered), BigInt::from(1));

        let mut clone = buffered.clone();
        assert_eq!(current_integer(&clone), BigInt::from(1));

        assert!(buffered.move_next());
        assert_eq!(current_integer(&buffered), BigInt::from(2));

        assert!(clone.move_next());
        assert_eq!(current_integer(&clone), BigInt::from(2));
    }
}
