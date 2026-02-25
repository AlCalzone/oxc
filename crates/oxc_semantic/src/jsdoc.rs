use rustc_hash::FxHashMap;

use oxc_jsdoc::JSDoc;
use oxc_span::{GetSpan, Span};

use crate::{AstNode, AstNodes};

#[derive(Debug, Default)]
pub struct JSDocFinder<'a> {
    /// JSDocs by Span
    attached: FxHashMap<u32, Vec<JSDoc<'a>>>,
    not_attached: Vec<JSDoc<'a>>,
}

impl<'a> JSDocFinder<'a> {
    pub fn new(attached: FxHashMap<u32, Vec<JSDoc<'a>>>, not_attached: Vec<JSDoc<'a>>) -> Self {
        Self { attached, not_attached }
    }

    pub fn get_one_by_node<'b>(
        &'b self,
        nodes: &AstNodes<'a>,
        node: &AstNode<'a>,
    ) -> Option<JSDoc<'a>> {
        let jsdocs = self.get_all_by_node(nodes, node)?;

        // If flagged, at least 1 JSDoc is attached
        // If multiple JSDocs are attached, return the last = nearest
        jsdocs.last().cloned()
    }

    pub fn get_all_by_node<'b>(
        &'b self,
        nodes: &AstNodes<'a>,
        node: &AstNode<'a>,
    ) -> Option<Vec<JSDoc<'a>>> {
        if !nodes.flags(node.id()).has_jsdoc() {
            return None;
        }

        let span = node.kind().span();
        self.get_all_by_span(span)
    }

    pub fn get_all_by_span<'b>(&'b self, span: Span) -> Option<Vec<JSDoc<'a>>> {
        self.attached.get(&span.start).cloned()
    }

    pub fn iter_all<'b>(&'b self) -> impl Iterator<Item = &'b JSDoc<'a>> + 'b {
        self.attached.values().flatten().chain(self.not_attached.iter())
    }
}

#[cfg(test)]
mod test {
    use oxc_allocator::Allocator;
    use oxc_jsdoc::JSDoc;
    use oxc_parser::Parser;
    use oxc_span::{SourceType, Span};

    use crate::{Semantic, SemanticBuilder};

    fn build_semantic<'a>(
        allocator: &'a Allocator,
        source_text: &'a str,
        source_type: Option<SourceType>,
    ) -> Semantic<'a> {
        let source_type = source_type.unwrap_or_default();
        let ret = Parser::new(allocator, source_text, source_type).parse();
        SemanticBuilder::new().build(allocator.alloc(ret.program)).semantic
    }

    fn get_jsdocs<'a>(
        allocator: &'a Allocator,
        source_text: &'a str,
        symbol: &'a str,
        source_type: Option<SourceType>,
    ) -> Option<Vec<JSDoc<'a>>> {
        let semantic = build_semantic(allocator, source_text, source_type);
        let start = u32::try_from(source_text.find(symbol).unwrap_or(0)).unwrap();
        let span = Span::sized(start, u32::try_from(symbol.len()).unwrap());
        semantic.jsdoc().get_all_by_span(span)
    }

    fn test_jsdoc_found(source_text: &str, symbol: &str, source_type: Option<SourceType>) {
        let allocator = Allocator::default();
        assert!(
            get_jsdocs(&allocator, source_text, symbol, source_type).is_some(),
            "JSDoc should found for\n  {symbol} \nin\n  {source_text}"
        );
    }

    fn test_jsdoc_not_found(source_text: &str, symbol: &str) {
        let allocator = Allocator::default();
        assert!(
            get_jsdocs(&allocator, source_text, symbol, None).is_none(),
            "JSDoc should NOT found for\n  {symbol} \nin\n  {source_text}"
        );
    }

    #[test]
    fn not_found() {
        let source_texts = [
            ("function f1() {}", "function f1() {}"),
            ("// test", "function f2() {}"),
            ("/* test */function f3() {}", "function f3() {}"),
            ("/** for 4a */ ; function f4a() {} function f4b() {}", "function f4b() {}"),
            ("function f4a() {} /** for 4b */ ; function f4b() {} ", "function f4a() {}"),
            ("function f5() {} /** test */", "function f5() {}"),
            (
                "/** for o */
                const o = {
                    f6() {}
                };",
                "f6() {}",
            ),
            ("/** for () */ (() => {})", "() => {}"),
            ("/** for ; */ ; let v1;", "let v1;"),
            ("/** for let v2 */ let v2 = () => {};", "() => {}"),
            ("/** for if */ if (true) { let v3; })", "let v3;"),
            (
                "class C1 {
                    /** for m1 */
                    m1() {}
                    m2() {}
                }",
                "m2() {}",
            ),
        ];
        for (source_text, target) in source_texts {
            test_jsdoc_not_found(source_text, target);
        }
    }

    #[test]
    fn found() {
        let source_texts = [
            ("/** test */function f1() {}", "function f1() {}"),
            ("/*** test */function f2() {}", "function f2() {}"),
            (
                "
            /** test */
        function f3() {}",
                "function f3() {}",
            ),
            (
                "/** test */


                function f4() {}",
                "function f4() {}",
            ),
            (
                "/**
             * test
             * */
            function f5() {}",
                "function f5() {}",
            ),
            (
                "/** test */
                // ---
                function f6() {}",
                "function f6() {}",
            ),
            (
                "/** test */
                /* -- */
                function f7() {}",
                "function f7() {}",
            ),
            (
                "/** test */
                /** test2 */
                function f8() {}",
                "function f8() {}",
            ),
            (
                "/** test */ /** test2 */
                function f9() {}",
                "function f9() {}",
            ),
            (
                "/** for f10 */ function f10() {} /** for f11 */ function f11() {}",
                "function f11() {}",
            ),
            (
                "const o = {
                    /** for f12 */
                    f12() {}
                };",
                "f12() {}",
            ),
            ("/** test */ (() => {})", "(() => {})"),
            ("/** test */ let v1 = 1", "let v1 = 1"),
            ("let v2a = 1, /** for v2b */ v2b = 2", "v2b = 2"),
            ("/** for v3a */ const v3a = 1, v3b = 2;", "const v3a = 1, v3b = 2;"),
            ("/** test */ export const e1 = 1;", "export const e1 = 1;"),
            ("/** test */ export default {};", "export default {};"),
            ("/** test */ import 'i1'", "import 'i1'"),
            ("/** test */ import I from 'i2'", "import I from 'i2'"),
            ("/** test */ import { I } from 'i3'", "import { I } from 'i3'"),
        ];
        for (source_text, target) in source_texts {
            test_jsdoc_found(source_text, target, None);
        }
    }

    #[test]
    fn found_ts() {
        let source_texts = [(
            "class Foo {
            /** jsdoc */
            bar: string;
        }",
            "bar: string;",
        )];

        let source_type = SourceType::default().with_typescript(true);
        for (source_text, target) in source_texts {
            test_jsdoc_found(source_text, target, Some(source_type));
        }
    }

    #[test]
    fn get_all_by_span_order() {
        let allocator = Allocator::default();
        let source_text = r"
            /**c0*/
            function foo() {}

            /**c1*/
            /* noop */
            /**c2*/
            // noop
            /**c3*/
            const x = () => {};
        ";
        let symbol = "const x = () => {};";
        let jsdocs = get_jsdocs(&allocator, source_text, symbol, None);

        assert!(jsdocs.is_some());
        let jsdocs = jsdocs.unwrap();
        assert_eq!(jsdocs.len(), 3);

        // Should be [farthest, ..., nearest]
        let mut iter = jsdocs.iter();
        let c1 = iter.next().unwrap();
        assert_eq!(c1.comment().parsed(), "c1");
        let c2 = iter.next().unwrap();
        assert_eq!(c2.comment().parsed(), "c2");
        let c3 = iter.next().unwrap();
        assert_eq!(c3.comment().parsed(), "c3");
    }

    #[test]
    fn get_all_jsdoc() {
        let allocator = Allocator::default();
        let semantic = build_semantic(
            &allocator,
            r"
            // noop
            /** 1. ; */
            // noop
            ;
            /** 2. class X {} *//** 3. class X {} */
            class X {
                /** 4. foo */
                foo = /** 5. () */ (() => {});
            }

            /** 6. let x; */
            /* noop */

            let x;

            /**/ // noop and noop

            /** 7. Not attached but collected! */
            ",
            Some(SourceType::default()),
        );
        assert_eq!(semantic.jsdoc().iter_all().count(), 7);
    }
}
