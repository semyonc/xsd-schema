//! XPath axis iterators aligned with the C# XPath20Api implementation.
//!
//! This module focuses on navigation-only iterators that operate on node
//! sequences and apply `NodeTest` filters.

use super::context::XPathContext;
use super::error::XPathError;
use super::iterator::{XmlItemRef, XmlNodeIterator};
use super::node_test::NodeTest;
use super::{DomNavigator, DomNodeType, NamespaceAxisScope};

use crate::types::ItemType;

fn move_to_next_document_order<N: DomNavigator>(nav: &mut N) -> bool {
    if nav.move_to_first_child() {
        return true;
    }
    if nav.move_to_next_sibling() {
        return true;
    }
    while nav.move_to_parent() {
        if nav.move_to_next_sibling() {
            return true;
        }
    }
    false
}

fn move_to_next_kind<N: DomNavigator>(nav: &mut N, kind: DomNodeType) -> bool {
    let mut cursor = nav.clone();
    while cursor.move_to_next_sibling() {
        if kind == DomNodeType::All || cursor.node_type() == kind {
            nav.move_to(&cursor);
            return true;
        }
    }
    false
}

fn move_to_first_kind<N: DomNavigator>(nav: &mut N, kind: DomNodeType) -> bool {
    if kind == DomNodeType::All {
        nav.move_to_first_child()
    } else {
        nav.move_to_child_kind(kind)
    }
}

fn move_to_next_kind_or_sibling<N: DomNavigator>(nav: &mut N, kind: DomNodeType) -> bool {
    if kind == DomNodeType::All {
        nav.move_to_next_sibling()
    } else {
        move_to_next_kind(nav, kind)
    }
}

fn node_test_kind(node_test: &Option<NodeTest>) -> DomNodeType {
    match node_test {
        None => DomNodeType::All,
        Some(NodeTest::Name(_)) => DomNodeType::Element,
        Some(NodeTest::Type(seq)) => match &seq.item_type {
            ItemType::AnyItem | ItemType::AnyNode => DomNodeType::All,
            ItemType::Document(_) => DomNodeType::Root,
            ItemType::Element(_, _) | ItemType::SchemaElement(_) => DomNodeType::Element,
            ItemType::Attribute(_, _) | ItemType::SchemaAttribute(_) => DomNodeType::Attribute,
            ItemType::NamespaceNode => DomNodeType::Namespace,
            ItemType::Text => DomNodeType::Text,
            ItemType::Comment => DomNodeType::Comment,
            ItemType::ProcessingInstruction(_) => DomNodeType::ProcessingInstruction,
            ItemType::AtomicType(_) | ItemType::SchemaAtomicType(_) => DomNodeType::All,
        },
    }
}

fn descendant_element_kind(node_test: &Option<NodeTest>) -> DomNodeType {
    match node_test {
        Some(NodeTest::Name(_)) => DomNodeType::Element,
        Some(NodeTest::Type(seq)) => match &seq.item_type {
            ItemType::Element(_, _) | ItemType::SchemaElement(_) => DomNodeType::Element,
            _ => DomNodeType::All,
        },
        None => DomNodeType::All,
    }
}

#[derive(Clone)]
struct AxisNodeIteratorBase<'a, I: XmlNodeIterator> {
    context: XPathContext<'a>,
    node_test: Option<NodeTest>,
    match_self: bool,
    iter: I,
    curr: Option<I::Navigator>,
    sequential_position: usize,
    accept: bool,
}

impl<'a, I: XmlNodeIterator> AxisNodeIteratorBase<'a, I> {
    fn new(
        context: XPathContext<'a>,
        node_test: Option<NodeTest>,
        match_self: bool,
        iter: I,
    ) -> Self {
        Self {
            context,
            node_test,
            match_self,
            iter,
            curr: None,
            sequential_position: 0,
            accept: false,
        }
    }

    fn test_item(&self, nav: &I::Navigator) -> bool {
        match &self.node_test {
            Some(test) => test.matches(nav, &self.context),
            None => true,
        }
    }

    fn move_next_iter(&mut self) -> Result<bool, XPathError> {
        if !self.iter.move_next()? {
            return Ok(false);
        }

        let item = match self.iter.current() {
            Some(item) => item,
            None => return Ok(false),
        };

        let nav = match item {
            XmlItemRef::Node(node) => node.clone(),
            XmlItemRef::Atomic(_) => return Err(XPathError::XPTY0019),
        };

        self.curr = Some(nav);
        self.sequential_position = 0;
        self.accept = true;
        Ok(true)
    }
}

pub trait AxisTraversal<N: DomNavigator>: Clone {
    fn move_to_first(&self, nav: &mut N) -> bool;
    fn move_to_next(&self, nav: &mut N) -> bool;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SelfAxis;

impl<N: DomNavigator> AxisTraversal<N> for SelfAxis {
    fn move_to_first(&self, _nav: &mut N) -> bool {
        true
    }

    fn move_to_next(&self, _nav: &mut N) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ParentAxis;

impl<N: DomNavigator> AxisTraversal<N> for ParentAxis {
    fn move_to_first(&self, nav: &mut N) -> bool {
        nav.move_to_parent()
    }

    fn move_to_next(&self, _nav: &mut N) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AncestorAxis;

impl<N: DomNavigator> AxisTraversal<N> for AncestorAxis {
    fn move_to_first(&self, nav: &mut N) -> bool {
        nav.move_to_parent()
    }

    fn move_to_next(&self, nav: &mut N) -> bool {
        nav.move_to_parent()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ChildAxis;

impl<N: DomNavigator> AxisTraversal<N> for ChildAxis {
    fn move_to_first(&self, nav: &mut N) -> bool {
        nav.move_to_first_child()
    }

    fn move_to_next(&self, nav: &mut N) -> bool {
        nav.move_to_next_sibling()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AttributeAxis;

impl<N: DomNavigator> AxisTraversal<N> for AttributeAxis {
    fn move_to_first(&self, nav: &mut N) -> bool {
        nav.move_to_first_attribute()
    }

    fn move_to_next(&self, nav: &mut N) -> bool {
        nav.move_to_next_attribute()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NamespaceAxis {
    scope: NamespaceAxisScope,
}

impl NamespaceAxis {
    pub fn new(scope: NamespaceAxisScope) -> Self {
        Self { scope }
    }
}

impl Default for NamespaceAxis {
    fn default() -> Self {
        Self {
            scope: NamespaceAxisScope::All,
        }
    }
}

impl<N: DomNavigator> AxisTraversal<N> for NamespaceAxis {
    fn move_to_first(&self, nav: &mut N) -> bool {
        nav.move_to_first_namespace(self.scope)
    }

    fn move_to_next(&self, nav: &mut N) -> bool {
        nav.move_to_next_namespace(self.scope)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FollowingSiblingAxis;

impl<N: DomNavigator> AxisTraversal<N> for FollowingSiblingAxis {
    fn move_to_first(&self, nav: &mut N) -> bool {
        nav.move_to_next_sibling()
    }

    fn move_to_next(&self, nav: &mut N) -> bool {
        nav.move_to_next_sibling()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PrecedingSiblingAxis;

impl<N: DomNavigator> AxisTraversal<N> for PrecedingSiblingAxis {
    fn move_to_first(&self, nav: &mut N) -> bool {
        nav.move_to_prev_sibling()
    }

    fn move_to_next(&self, nav: &mut N) -> bool {
        nav.move_to_prev_sibling()
    }
}

#[derive(Clone)]
pub struct SequentialAxisNodeIterator<'a, I, A>
where
    I: XmlNodeIterator,
    A: AxisTraversal<I::Navigator>,
{
    base: AxisNodeIteratorBase<'a, I>,
    axis: A,
    first: bool,
    index: Option<usize>,
}

impl<'a, I, A> SequentialAxisNodeIterator<'a, I, A>
where
    I: XmlNodeIterator,
    A: AxisTraversal<I::Navigator>,
{
    pub fn new(
        context: XPathContext<'a>,
        node_test: Option<NodeTest>,
        match_self: bool,
        iter: I,
        axis: A,
    ) -> Self {
        Self {
            base: AxisNodeIteratorBase::new(context, node_test, match_self, iter),
            axis,
            first: false,
            index: None,
        }
    }

    fn next_item(&mut self) -> Result<bool, XPathError> {
        loop {
            if !self.base.accept {
                if !self.base.move_next_iter()? {
                    return Ok(false);
                }
                self.first = true;
                if self.base.match_self {
                    if let Some(curr) = self.base.curr.as_ref() {
                        if self.base.test_item(curr) {
                            self.base.sequential_position += 1;
                            return Ok(true);
                        }
                    }
                }
            }

            let moved = if self.first {
                self.first = false;
                match self.base.curr.as_mut() {
                    Some(nav) => self.axis.move_to_first(nav),
                    None => false,
                }
            } else {
                match self.base.curr.as_mut() {
                    Some(nav) => self.axis.move_to_next(nav),
                    None => false,
                }
            };

            self.base.accept = moved;

            if moved {
                if let Some(curr) = self.base.curr.as_ref() {
                    if self.base.test_item(curr) {
                        self.base.sequential_position += 1;
                        return Ok(true);
                    }
                }
            }
        }
    }
}

impl<'a, I, A> XmlNodeIterator for SequentialAxisNodeIterator<'a, I, A>
where
    I: XmlNodeIterator,
    A: AxisTraversal<I::Navigator>,
{
    type Navigator = I::Navigator;

    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>> {
        self.base.curr.as_ref().map(XmlItemRef::Node)
    }

    fn current_position(&self) -> Option<usize> {
        self.index
    }

    fn move_next(&mut self) -> Result<bool, XPathError> {
        if self.next_item()? {
            let next_index = match self.index {
                None => 0,
                Some(i) => i + 1,
            };
            self.index = Some(next_index);
            Ok(true)
        } else {
            self.index = None;
            self.base.curr = None;
            Ok(false)
        }
    }

    fn sequential_position(&self) -> Option<usize> {
        self.current_position()
            .map(|_| self.base.sequential_position)
    }

    fn reset_sequential_position(&mut self) {
        self.base.accept = false;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SpecialChildAxis {
    kind: DomNodeType,
}

impl SpecialChildAxis {
    pub fn new(kind: DomNodeType) -> Self {
        Self { kind }
    }
}

impl<N: DomNavigator> AxisTraversal<N> for SpecialChildAxis {
    fn move_to_first(&self, nav: &mut N) -> bool {
        nav.move_to_child_kind(self.kind)
    }

    fn move_to_next(&self, nav: &mut N) -> bool {
        move_to_next_kind(nav, self.kind)
    }
}

#[derive(Clone)]
pub struct SpecialChildNodeIterator<'a, I: XmlNodeIterator> {
    inner: SequentialAxisNodeIterator<'a, I, SpecialChildAxis>,
}

impl<'a, I: XmlNodeIterator> SpecialChildNodeIterator<'a, I> {
    pub fn new(context: XPathContext<'a>, node_test: Option<NodeTest>, iter: I) -> Self {
        let kind = node_test_kind(&node_test);
        let axis = SpecialChildAxis::new(kind);
        Self {
            inner: SequentialAxisNodeIterator::new(context, node_test, false, iter, axis),
        }
    }
}

impl<'a, I: XmlNodeIterator> XmlNodeIterator for SpecialChildNodeIterator<'a, I> {
    type Navigator = I::Navigator;

    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>> {
        self.inner.current()
    }

    fn current_position(&self) -> Option<usize> {
        self.inner.current_position()
    }

    fn move_next(&mut self) -> Result<bool, XPathError> {
        self.inner.move_next()
    }

    fn sequential_position(&self) -> Option<usize> {
        self.inner.sequential_position()
    }

    fn reset_sequential_position(&mut self) {
        self.inner.reset_sequential_position();
    }
}

#[derive(Clone)]
pub struct DescendantNodeIterator<'a, I: XmlNodeIterator> {
    base: AxisNodeIteratorBase<'a, I>,
    depth: usize,
    index: Option<usize>,
}

impl<'a, I: XmlNodeIterator> DescendantNodeIterator<'a, I> {
    pub fn new(
        context: XPathContext<'a>,
        node_test: Option<NodeTest>,
        match_self: bool,
        iter: I,
    ) -> Self {
        Self {
            base: AxisNodeIteratorBase::new(context, node_test, match_self, iter),
            depth: 0,
            index: None,
        }
    }

    fn next_item(&mut self) -> Result<bool, XPathError> {
        loop {
            if !self.base.accept {
                if !self.base.move_next_iter()? {
                    return Ok(false);
                }
                if self.base.match_self {
                    if let Some(curr) = self.base.curr.as_ref() {
                        if self.base.test_item(curr) {
                            self.base.sequential_position += 1;
                            return Ok(true);
                        }
                    }
                }
            }

            let moved_to_child = match self.base.curr.as_mut() {
                Some(nav) => nav.move_to_first_child(),
                None => false,
            };

            if moved_to_child {
                self.depth += 1;
            } else {
                loop {
                    if self.depth == 0 {
                        self.base.accept = false;
                        break;
                    }
                    let moved_to_sibling = match self.base.curr.as_mut() {
                        Some(nav) => nav.move_to_next_sibling(),
                        None => false,
                    };
                    if moved_to_sibling {
                        break;
                    }
                    if let Some(nav) = self.base.curr.as_mut() {
                        nav.move_to_parent();
                    }
                    if self.depth > 0 {
                        self.depth -= 1;
                    }
                }
                if !self.base.accept {
                    continue;
                }
            }

            if let Some(curr) = self.base.curr.as_ref() {
                if self.base.test_item(curr) {
                    self.base.sequential_position += 1;
                    return Ok(true);
                }
            }
        }
    }
}

impl<'a, I: XmlNodeIterator> XmlNodeIterator for DescendantNodeIterator<'a, I> {
    type Navigator = I::Navigator;

    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>> {
        self.base.curr.as_ref().map(XmlItemRef::Node)
    }

    fn current_position(&self) -> Option<usize> {
        self.index
    }

    fn move_next(&mut self) -> Result<bool, XPathError> {
        if self.next_item()? {
            let next_index = match self.index {
                None => 0,
                Some(i) => i + 1,
            };
            self.index = Some(next_index);
            Ok(true)
        } else {
            self.index = None;
            self.base.curr = None;
            Ok(false)
        }
    }

    fn sequential_position(&self) -> Option<usize> {
        self.current_position().map(|_| self.base.sequential_position)
    }

    fn reset_sequential_position(&mut self) {
        self.base.accept = false;
    }
}

#[derive(Clone)]
pub struct SpecialDescendantNodeIterator<'a, I: XmlNodeIterator> {
    base: AxisNodeIteratorBase<'a, I>,
    kind: DomNodeType,
    depth: usize,
    index: Option<usize>,
}

impl<'a, I: XmlNodeIterator> SpecialDescendantNodeIterator<'a, I> {
    pub fn new(
        context: XPathContext<'a>,
        node_test: Option<NodeTest>,
        match_self: bool,
        iter: I,
    ) -> Self {
        let kind = descendant_element_kind(&node_test);
        Self {
            base: AxisNodeIteratorBase::new(context, node_test, match_self, iter),
            kind,
            depth: 0,
            index: None,
        }
    }

    fn next_item(&mut self) -> Result<bool, XPathError> {
        loop {
            if !self.base.accept {
                if !self.base.move_next_iter()? {
                    return Ok(false);
                }
                self.depth = 0;
                if self.base.match_self {
                    if let Some(curr) = self.base.curr.as_ref() {
                        if self.base.test_item(curr) {
                            self.base.sequential_position += 1;
                            return Ok(true);
                        }
                    }
                }
            }

            let kind = self.kind;
            let moved_to_child = match self.base.curr.as_mut() {
                Some(nav) => move_to_first_kind(nav, kind),
                None => false,
            };

            if moved_to_child {
                self.depth += 1;
            } else {
                loop {
                    if self.depth == 0 {
                        self.base.accept = false;
                        break;
                    }
                    let moved_to_next = match self.base.curr.as_mut() {
                        Some(nav) => move_to_next_kind_or_sibling(nav, kind),
                        None => false,
                    };
                    if moved_to_next {
                        break;
                    }
                    if let Some(nav) = self.base.curr.as_mut() {
                        nav.move_to_parent();
                    }
                    if self.depth > 0 {
                        self.depth -= 1;
                    }
                }
                if !self.base.accept {
                    continue;
                }
            }

            if let Some(curr) = self.base.curr.as_ref() {
                if self.base.test_item(curr) {
                    self.base.sequential_position += 1;
                    return Ok(true);
                }
            }
        }
    }
}

impl<'a, I: XmlNodeIterator> XmlNodeIterator for SpecialDescendantNodeIterator<'a, I> {
    type Navigator = I::Navigator;

    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>> {
        self.base.curr.as_ref().map(XmlItemRef::Node)
    }

    fn current_position(&self) -> Option<usize> {
        self.index
    }

    fn move_next(&mut self) -> Result<bool, XPathError> {
        if self.next_item()? {
            let next_index = match self.index {
                None => 0,
                Some(i) => i + 1,
            };
            self.index = Some(next_index);
            Ok(true)
        } else {
            self.index = None;
            self.base.curr = None;
            Ok(false)
        }
    }

    fn sequential_position(&self) -> Option<usize> {
        self.current_position().map(|_| self.base.sequential_position)
    }

    fn reset_sequential_position(&mut self) {
        self.base.accept = false;
    }
}

#[derive(Clone)]
pub struct ChildOverDescendantsNodeIterator<'a, I: XmlNodeIterator> {
    context: XPathContext<'a>,
    node_tests: Vec<NodeTest>,
    last_test: NodeTest,
    iter: I,
    curr: Option<I::Navigator>,
    kind: DomNodeType,
    depth: usize,
    accept: bool,
    sequential_position: usize,
    index: Option<usize>,
}

impl<'a, I: XmlNodeIterator> ChildOverDescendantsNodeIterator<'a, I> {
    pub fn new(context: XPathContext<'a>, node_tests: Vec<NodeTest>, iter: I) -> Self {
        let last_test = node_tests
            .last()
            .expect("ChildOverDescendants requires at least one NodeTest")
            .clone();
        let kind = descendant_element_kind(&Some(last_test.clone()));
        Self {
            context,
            node_tests,
            last_test,
            iter,
            curr: None,
            kind,
            depth: 0,
            accept: false,
            sequential_position: 0,
            index: None,
        }
    }

    fn test_item(&self, nav: &I::Navigator, test: &NodeTest) -> bool {
        test.matches(nav, &self.context)
    }

    fn next_item(&mut self) -> Result<bool, XPathError> {
        loop {
            if !self.accept {
                if !self.iter.move_next()? {
                    return Ok(false);
                }
                let item = match self.iter.current() {
                    Some(item) => item,
                    None => return Ok(false),
                };
                let nav = match item {
                    XmlItemRef::Node(node) => node.clone(),
                    XmlItemRef::Atomic(_) => return Err(XPathError::XPTY0019),
                };
                self.curr = Some(nav);
                self.depth = 0;
                self.sequential_position = 0;
                self.accept = true;
            }

            let kind = self.kind;
            let moved_to_child = match self.curr.as_mut() {
                Some(nav) => move_to_first_kind(nav, kind),
                None => false,
            };
            if moved_to_child {
                self.depth += 1;
            } else {
                loop {
                    if self.depth == 0 {
                        self.accept = false;
                        break;
                    }
                    let moved_to_next = match self.curr.as_mut() {
                        Some(nav) => move_to_next_kind_or_sibling(nav, kind),
                        None => false,
                    };
                    if moved_to_next {
                        break;
                    }
                    if let Some(nav) = self.curr.as_mut() {
                        nav.move_to_parent();
                    }
                    if self.depth > 0 {
                        self.depth -= 1;
                    }
                }
                if !self.accept {
                    continue;
                }
            }

            let curr = match self.curr.as_ref() {
                Some(curr) => curr,
                None => continue,
            };

            if self.depth < self.node_tests.len() || !self.test_item(curr, &self.last_test) {
                continue;
            }

            let mut nav = curr.clone();
            let mut matched = true;
            for test in self.node_tests[..self.node_tests.len() - 1]
                .iter()
                .rev()
            {
                if !(nav.move_to_parent() && self.test_item(&nav, test)) {
                    matched = false;
                    break;
                }
            }
            if !matched {
                continue;
            }
            self.sequential_position += 1;
            return Ok(true);
        }
    }
}

impl<'a, I: XmlNodeIterator> XmlNodeIterator for ChildOverDescendantsNodeIterator<'a, I> {
    type Navigator = I::Navigator;

    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>> {
        self.curr.as_ref().map(XmlItemRef::Node)
    }

    fn current_position(&self) -> Option<usize> {
        self.index
    }

    fn move_next(&mut self) -> Result<bool, XPathError> {
        if self.next_item()? {
            let next_index = match self.index {
                None => 0,
                Some(i) => i + 1,
            };
            self.index = Some(next_index);
            Ok(true)
        } else {
            self.index = None;
            self.curr = None;
            Ok(false)
        }
    }

    fn sequential_position(&self) -> Option<usize> {
        self.current_position().map(|_| self.sequential_position)
    }

    fn reset_sequential_position(&mut self) {
        self.accept = false;
    }
}

#[derive(Clone)]
pub struct FollowingNodeIterator<'a, I: XmlNodeIterator> {
    base: AxisNodeIteratorBase<'a, I>,
    kind: DomNodeType,
    index: Option<usize>,
}

impl<'a, I: XmlNodeIterator> FollowingNodeIterator<'a, I> {
    pub fn new(context: XPathContext<'a>, node_test: Option<NodeTest>, iter: I) -> Self {
        let kind = node_test_kind(&node_test);
        Self {
            base: AxisNodeIteratorBase::new(context, node_test, false, iter),
            kind,
            index: None,
        }
    }

    fn next_item(&mut self) -> Result<bool, XPathError> {
        loop {
            if !self.base.accept
                && !self.base.move_next_iter()? {
                    return Ok(false);
                }

            let moved = match self.base.curr.as_mut() {
                Some(nav) => nav.move_to_following(self.kind, None),
                None => false,
            };
            self.base.accept = moved;
            if moved {
                if let Some(curr) = self.base.curr.as_ref() {
                    if self.base.test_item(curr) {
                        self.base.sequential_position += 1;
                        return Ok(true);
                    }
                }
            }
        }
    }
}

impl<'a, I: XmlNodeIterator> XmlNodeIterator for FollowingNodeIterator<'a, I> {
    type Navigator = I::Navigator;

    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>> {
        self.base.curr.as_ref().map(XmlItemRef::Node)
    }

    fn current_position(&self) -> Option<usize> {
        self.index
    }

    fn move_next(&mut self) -> Result<bool, XPathError> {
        if self.next_item()? {
            let next_index = match self.index {
                None => 0,
                Some(i) => i + 1,
            };
            self.index = Some(next_index);
            Ok(true)
        } else {
            self.index = None;
            self.base.curr = None;
            Ok(false)
        }
    }

    fn sequential_position(&self) -> Option<usize> {
        self.current_position().map(|_| self.base.sequential_position)
    }

    fn reset_sequential_position(&mut self) {
        self.base.accept = false;
    }
}

#[derive(Clone)]
pub struct PrecedingNodeIterator<'a, I: XmlNodeIterator> {
    base: AxisNodeIteratorBase<'a, I>,
    kind: DomNodeType,
    anchor: Option<I::Navigator>,
    ancestors: Vec<I::Navigator>,
    started: bool,
    index: Option<usize>,
}

impl<'a, I: XmlNodeIterator> PrecedingNodeIterator<'a, I> {
    pub fn new(context: XPathContext<'a>, node_test: Option<NodeTest>, iter: I) -> Self {
        let kind = node_test_kind(&node_test);
        Self {
            base: AxisNodeIteratorBase::new(context, node_test, false, iter),
            kind,
            anchor: None,
            ancestors: Vec::new(),
            started: false,
            index: None,
        }
    }

    fn collect_ancestors(&mut self, anchor: &I::Navigator) {
        self.ancestors.clear();
        let mut cursor = anchor.clone();
        while cursor.move_to_parent() {
            self.ancestors.push(cursor.clone());
        }
    }

    fn is_ancestor(&self, nav: &I::Navigator) -> bool {
        self.ancestors
            .iter()
            .any(|ancestor| nav.is_same_position(ancestor))
    }

    fn next_item(&mut self) -> Result<bool, XPathError> {
        loop {
            if !self.base.accept {
                if !self.base.move_next_iter()? {
                    return Ok(false);
                }
                let mut anchor = match self.base.curr.as_ref() {
                    Some(nav) => nav.clone(),
                    None => return Ok(false),
                };
                if matches!(
                    anchor.node_type(),
                    DomNodeType::Attribute | DomNodeType::Namespace
                ) {
                    anchor.move_to_parent();
                }
                self.anchor = Some(anchor.clone());
                self.collect_ancestors(&anchor);
                if let Some(curr) = self.base.curr.as_mut() {
                    curr.move_to_root();
                }
                self.started = false;
            }

            let moved = match self.base.curr.as_mut() {
                Some(curr) => {
                    if self.started {
                        move_to_next_document_order(curr)
                    } else {
                        self.started = true;
                        curr.move_to_first_child()
                    }
                }
                None => false,
            };
            if !moved {
                self.base.accept = false;
                continue;
            }
            let curr = match self.base.curr.as_ref() {
                Some(curr) => curr,
                None => {
                    self.base.accept = false;
                    continue;
                }
            };
            if let Some(anchor) = self.anchor.as_ref() {
                if curr.is_same_position(anchor) {
                    self.base.accept = false;
                    continue;
                }
            }
            if self.kind != DomNodeType::All && curr.node_type() != self.kind {
                continue;
            }
            if self.is_ancestor(curr) {
                continue;
            }
            if self.base.test_item(curr) {
                self.base.sequential_position += 1;
                return Ok(true);
            }
        }
    }
}

impl<'a, I: XmlNodeIterator> XmlNodeIterator for PrecedingNodeIterator<'a, I> {
    type Navigator = I::Navigator;

    fn current(&self) -> Option<XmlItemRef<'_, Self::Navigator>> {
        self.base.curr.as_ref().map(XmlItemRef::Node)
    }

    fn current_position(&self) -> Option<usize> {
        self.index
    }

    fn move_next(&mut self) -> Result<bool, XPathError> {
        if self.next_item()? {
            let next_index = match self.index {
                None => 0,
                Some(i) => i + 1,
            };
            self.index = Some(next_index);
            Ok(true)
        } else {
            self.index = None;
            self.base.curr = None;
            Ok(false)
        }
    }

    fn sequential_position(&self) -> Option<usize> {
        self.current_position().map(|_| self.base.sequential_position)
    }

    fn reset_sequential_position(&mut self) {
        self.base.accept = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::namespace::table::NameTable;
    use crate::types::{ItemType, NameTest, SequenceType};
    use crate::xpath::iterator::{VecNodeIterator, XmlItem};
    use crate::navigator::RoXmlNavigator;

    fn collect_local_names<N: DomNavigator>(
        iter: &mut impl XmlNodeIterator<Navigator = N>,
    ) -> Vec<String> {
        let mut names = Vec::new();
        while iter.move_next().unwrap() {
            match iter.current() {
                Some(XmlItemRef::Node(node)) => names.push(node.local_name().to_string()),
                _ => panic!("expected node item"),
            }
        }
        names
    }

    #[test]
    fn test_self_axis() {
        let doc = roxmltree::Document::parse("<root><child/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter = SequentialAxisNodeIterator::new(
            ctx,
            Some(NodeTest::Type(SequenceType::node())),
            false,
            base,
            SelfAxis,
        );

        assert!(iter.move_next().unwrap());
        match iter.current() {
            Some(XmlItemRef::Node(node)) => assert_eq!(node.local_name(), "root"),
            _ => panic!("expected node"),
        }
        assert_eq!(iter.current_position(), Some(0));
        assert_eq!(iter.sequential_position(), Some(1));
        assert!(!iter.move_next().unwrap());
    }

    #[test]
    fn test_parent_axis() {
        let doc = roxmltree::Document::parse("<root><child/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();
        nav.move_to_first_child();

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter = SequentialAxisNodeIterator::new(
            ctx,
            Some(NodeTest::Type(SequenceType::node())),
            false,
            base,
            ParentAxis,
        );

        assert!(iter.move_next().unwrap());
        match iter.current() {
            Some(XmlItemRef::Node(node)) => assert_eq!(node.local_name(), "root"),
            _ => panic!("expected node"),
        }
        assert!(!iter.move_next().unwrap());
    }

    #[test]
    fn test_child_axis() {
        let doc = roxmltree::Document::parse("<root><a/><b/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter = SequentialAxisNodeIterator::new(
            ctx,
            Some(NodeTest::Name(NameTest::Wildcard)),
            false,
            base,
            ChildAxis,
        );

        let names = collect_local_names(&mut iter);
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_attribute_axis() {
        let doc = roxmltree::Document::parse("<root a=\"1\" b=\"2\"/>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child();

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter = SequentialAxisNodeIterator::new(
            ctx,
            Some(NodeTest::Name(NameTest::Wildcard)),
            false,
            base,
            AttributeAxis,
        );

        let mut names = collect_local_names(&mut iter);
        names.sort();
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_following_sibling_axis() {
        let doc = roxmltree::Document::parse("<root><a/><b/><c/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter = SequentialAxisNodeIterator::new(
            ctx,
            Some(NodeTest::Name(NameTest::Wildcard)),
            false,
            base,
            FollowingSiblingAxis,
        );

        let names = collect_local_names(&mut iter);
        assert_eq!(names, vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn test_preceding_sibling_axis() {
        let doc = roxmltree::Document::parse("<root><a/><b/><c/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a
        nav.move_to_next_sibling(); // b
        nav.move_to_next_sibling(); // c

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter = SequentialAxisNodeIterator::new(
            ctx,
            Some(NodeTest::Name(NameTest::Wildcard)),
            false,
            base,
            PrecedingSiblingAxis,
        );

        let names = collect_local_names(&mut iter);
        assert_eq!(names, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn test_ancestor_axis() {
        let doc = roxmltree::Document::parse("<root><a><b/></a></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a
        nav.move_to_first_child(); // b

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter = SequentialAxisNodeIterator::new(
            ctx,
            Some(NodeTest::Name(NameTest::Wildcard)),
            false,
            base,
            AncestorAxis,
        );

        let names = collect_local_names(&mut iter);
        assert_eq!(names, vec!["a".to_string(), "root".to_string()]);
    }

    #[test]
    fn test_namespace_axis() {
        let doc = roxmltree::Document::parse(
            r#"<root xmlns="urn:default" xmlns:p="urn:test"><p:child/></root>"#,
        )
        .expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter = SequentialAxisNodeIterator::new(
            ctx,
            Some(NodeTest::Type(SequenceType::one(ItemType::NamespaceNode))),
            false,
            base,
            NamespaceAxis::new(NamespaceAxisScope::Local),
        );

        let names = collect_local_names(&mut iter);
        assert!(names.contains(&"".to_string()));
        assert!(names.contains(&"p".to_string()));
    }

    #[test]
    fn test_descendant_axis() {
        let doc = roxmltree::Document::parse("<root><a><b/><c/></a><d/></root>")
            .expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter = DescendantNodeIterator::new(
            ctx,
            Some(NodeTest::Name(NameTest::Wildcard)),
            false,
            base,
        );

        let names = collect_local_names(&mut iter);
        assert_eq!(
            names,
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string()
            ]
        );
    }

    #[test]
    fn test_descendant_or_self_axis() {
        let doc = roxmltree::Document::parse("<root><a/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter = DescendantNodeIterator::new(
            ctx,
            Some(NodeTest::Name(NameTest::Wildcard)),
            true,
            base,
        );

        let names = collect_local_names(&mut iter);
        assert_eq!(names, vec!["root".to_string(), "a".to_string()]);
    }

    #[test]
    fn test_following_axis() {
        let doc = roxmltree::Document::parse("<root><a><b/><c/></a><d/></root>")
            .expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a
        nav.move_to_first_child(); // b

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter =
            FollowingNodeIterator::new(ctx, Some(NodeTest::Name(NameTest::Wildcard)), base);

        let names = collect_local_names(&mut iter);
        assert_eq!(names, vec!["c".to_string(), "d".to_string()]);
    }

    #[test]
    fn test_preceding_axis() {
        let doc = roxmltree::Document::parse("<root><a><b/><c/></a><d/></root>")
            .expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root
        nav.move_to_first_child(); // a
        nav.move_to_first_child(); // b
        nav.move_to_next_sibling(); // c
        nav.move_to_parent(); // a
        nav.move_to_next_sibling(); // d

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter =
            PrecedingNodeIterator::new(ctx, Some(NodeTest::Name(NameTest::Wildcard)), base);

        let names = collect_local_names(&mut iter);
        assert_eq!(
            names,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn test_special_child_axis() {
        let doc = roxmltree::Document::parse("<root>text<a/><b/></root>").expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter =
            SpecialChildNodeIterator::new(ctx, Some(NodeTest::Name(NameTest::Wildcard)), base);

        let names = collect_local_names(&mut iter);
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_special_descendant_axis() {
        let doc = roxmltree::Document::parse("<root>text<a><b/></a></root>")
            .expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        let ctx = XPathContext::new(&table);
        let mut iter = SpecialDescendantNodeIterator::new(
            ctx,
            Some(NodeTest::Name(NameTest::Wildcard)),
            false,
            base,
        );

        let names = collect_local_names(&mut iter);
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_child_over_descendants() {
        let doc = roxmltree::Document::parse(
            "<root><a><b><c/></b></a><x><b><c/></b></x></root>",
        )
        .expect("parse xml");
        let mut nav = RoXmlNavigator::new(&doc);
        nav.move_to_first_child(); // root

        let base = VecNodeIterator::new(vec![XmlItem::Node(nav.clone())]);
        let table = NameTable::new();
        // Intern the local names for matching
        let a_id = table.add("a");
        let b_id = table.add("b");
        let c_id = table.add("c");
        let ctx = XPathContext::new(&table);
        // NamespaceWildcard matches any namespace with specific local name (*:local)
        let tests = vec![
            NodeTest::Name(NameTest::NamespaceWildcard(a_id)),
            NodeTest::Name(NameTest::NamespaceWildcard(b_id)),
            NodeTest::Name(NameTest::NamespaceWildcard(c_id)),
        ];
        let mut iter = ChildOverDescendantsNodeIterator::new(ctx, tests, base);

        let names = collect_local_names(&mut iter);
        assert_eq!(names, vec!["c".to_string()]);
    }
}
