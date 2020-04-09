/*
 * Copyright © 2019-2020 Peter M. Stahl pemistahl@gmail.com
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either expressed or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::collections::BTreeSet;

use itertools::EitherOrBoth::Both;
use itertools::Itertools;
use ndarray::{Array1, Array2};
use petgraph::prelude::EdgeRef;

use crate::dfa::DFA;
use crate::grapheme::{Grapheme, GraphemeCluster};
use crate::regexp::RegExpConfig;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Expression<'a> {
    Alternation(Vec<Expression<'a>>, &'a RegExpConfig),
    CharacterClass(BTreeSet<char>, &'a RegExpConfig),
    Concatenation(Box<Expression<'a>>, Box<Expression<'a>>, &'a RegExpConfig),
    Literal(GraphemeCluster, &'a RegExpConfig),
    Repetition(Box<Expression<'a>>, Quantifier, &'a RegExpConfig),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Quantifier {
    KleeneStar,
    QuestionMark,
}

pub(crate) enum Substring {
    Prefix,
    Suffix,
}

impl<'a> Expression<'a> {
    pub(crate) fn from(dfa: DFA, config: &'a RegExpConfig) -> Self {
        let states = dfa.states_in_depth_first_order();
        let state_count = dfa.state_count();

        let mut a = Array2::<Option<Expression>>::default((state_count, state_count));
        let mut b = Array1::<Option<Expression>>::default(state_count);

        for (i, state) in states.iter().enumerate() {
            if dfa.is_final_state(*state) {
                b[i] = Some(Expression::new_literal(
                    GraphemeCluster::from("", config),
                    config,
                ));
            }

            for edge in dfa.outgoing_edges(*state) {
                let grapheme = edge.weight();
                let literal =
                    Expression::new_literal(GraphemeCluster::new(grapheme.clone()), config);
                let j = states.iter().position(|&it| it == edge.target()).unwrap();

                a[(i, j)] = if a[(i, j)].is_some() {
                    Self::union(&a[(i, j)], &Some(literal), config)
                } else {
                    Some(literal)
                }
            }
        }

        for n in (0..state_count).rev() {
            if a[(n, n)].is_some() {
                b[n] = Self::concatenate(
                    &Self::repeat_zero_or_more_times(&a[(n, n)], config),
                    &b[n],
                    config,
                );
                for j in 0..n {
                    a[(n, j)] = Self::concatenate(
                        &Self::repeat_zero_or_more_times(&a[(n, n)], config),
                        &a[(n, j)],
                        config,
                    );
                }
            }

            for i in 0..n {
                if a[(i, n)].is_some() {
                    b[i] =
                        Self::union(&b[i], &Self::concatenate(&a[(i, n)], &b[n], config), config);
                    for j in 0..n {
                        a[(i, j)] = Self::union(
                            &a[(i, j)],
                            &Self::concatenate(&a[(i, n)], &a[(n, j)], config),
                            config,
                        );
                    }
                }
            }
        }

        if !b.is_empty() && b[0].is_some() {
            b[0].as_ref().unwrap().clone()
        } else {
            Expression::new_literal(GraphemeCluster::from("", config), config)
        }
    }

    fn new_alternation(
        expr1: Expression<'a>,
        expr2: Expression<'a>,
        config: &'a RegExpConfig,
    ) -> Self {
        let mut options: Vec<Expression> = vec![];
        Self::flatten_alternations(&mut options, vec![expr1, expr2]);
        options.sort_by(|a, b| b.len().cmp(&a.len()));
        Expression::Alternation(options, config)
    }

    fn new_character_class(
        first_char_set: BTreeSet<char>,
        second_char_set: BTreeSet<char>,
        config: &'a RegExpConfig,
    ) -> Self {
        let union_set = first_char_set.union(&second_char_set).copied().collect();
        Expression::CharacterClass(union_set, config)
    }

    fn new_concatenation(
        expr1: Expression<'a>,
        expr2: Expression<'a>,
        config: &'a RegExpConfig,
    ) -> Self {
        Expression::Concatenation(Box::from(expr1), Box::from(expr2), config)
    }

    fn new_literal(cluster: GraphemeCluster, config: &'a RegExpConfig) -> Self {
        Expression::Literal(cluster, config)
    }

    fn new_repetition(
        expr: Expression<'a>,
        quantifier: Quantifier,
        config: &'a RegExpConfig,
    ) -> Self {
        Expression::Repetition(Box::from(expr), quantifier, config)
    }

    fn is_empty(&self) -> bool {
        match self {
            Expression::Literal(cluster, _) => cluster.is_empty(),
            _ => false,
        }
    }

    pub(crate) fn is_single_codepoint(&self) -> bool {
        match self {
            Expression::CharacterClass(_, _) => true,
            Expression::Literal(cluster, config) => {
                cluster.char_count(config.is_non_ascii_char_escaped) == 1
                    && cluster.graphemes().first().unwrap().maximum() == 1
            }
            _ => false,
        }
    }

    fn len(&self) -> usize {
        match self {
            Expression::Alternation(options, _) => options.first().unwrap().len(),
            Expression::CharacterClass(_, _) => 1,
            Expression::Concatenation(expr1, expr2, _) => expr1.len() + expr2.len(),
            Expression::Literal(cluster, _) => cluster.size(),
            Expression::Repetition(expr, _, _) => expr.len(),
        }
    }

    pub(crate) fn precedence(&self) -> u8 {
        match self {
            Expression::Alternation(_, _) | Expression::CharacterClass(_, _) => 1,
            Expression::Concatenation(_, _, _) | Expression::Literal(_, _) => 2,
            Expression::Repetition(_, _, _) => 3,
        }
    }

    pub(crate) fn remove_substring(&mut self, substring: &Substring, length: usize) {
        match self {
            Expression::Concatenation(expr1, expr2, _) => match substring {
                Substring::Prefix => {
                    if let Expression::Literal(_, _) = **expr1 {
                        expr1.remove_substring(substring, length)
                    }
                }
                Substring::Suffix => {
                    if let Expression::Literal(_, _) = **expr2 {
                        expr2.remove_substring(substring, length)
                    }
                }
            },
            Expression::Literal(cluster, _) => match substring {
                Substring::Prefix => {
                    cluster.graphemes_mut().drain(..length);
                }
                Substring::Suffix => {
                    let graphemes = cluster.graphemes_mut();
                    graphemes.drain(graphemes.len() - length..);
                }
            },
            _ => (),
        }
    }

    pub(crate) fn value(&self, substring: Option<&Substring>) -> Option<Vec<Grapheme>> {
        match self {
            Expression::Concatenation(expr1, expr2, _) => match substring {
                Some(value) => match value {
                    Substring::Prefix => expr1.value(None),
                    Substring::Suffix => expr2.value(None),
                },
                None => None,
            },
            Expression::Literal(cluster, _) => Some(cluster.graphemes().clone()),
            _ => None,
        }
    }

    fn repeat_zero_or_more_times(
        expr: &Option<Expression<'a>>,
        config: &'a RegExpConfig,
    ) -> Option<Expression<'a>> {
        if let Some(value) = expr {
            Some(Expression::new_repetition(
                value.clone(),
                Quantifier::KleeneStar,
                config,
            ))
        } else {
            None
        }
    }

    fn concatenate(
        a: &Option<Expression<'a>>,
        b: &Option<Expression<'a>>,
        config: &'a RegExpConfig,
    ) -> Option<Expression<'a>> {
        if a.is_none() || b.is_none() {
            return None;
        }

        let expr1 = a.as_ref().unwrap();
        let expr2 = b.as_ref().unwrap();

        if expr1.is_empty() {
            return b.clone();
        }
        if expr2.is_empty() {
            return a.clone();
        }

        if let (Expression::Literal(graphemes_a, config), Expression::Literal(graphemes_b, _)) =
            (&expr1, &expr2)
        {
            return Some(Expression::new_literal(
                GraphemeCluster::merge(graphemes_a, graphemes_b),
                config,
            ));
        }

        if let (Expression::Literal(graphemes_a, _), Expression::Concatenation(first, second, _)) =
            (&expr1, &expr2)
        {
            if let Expression::Literal(graphemes_first, config) = &**first {
                let literal = Expression::new_literal(
                    GraphemeCluster::merge(graphemes_a, graphemes_first),
                    config,
                );
                return Some(Expression::new_concatenation(
                    literal,
                    *second.clone(),
                    config,
                ));
            }
        }

        if let (Expression::Literal(graphemes_b, _), Expression::Concatenation(first, second, _)) =
            (&expr2, &expr1)
        {
            if let Expression::Literal(graphemes_second, config) = &**second {
                let literal = Expression::new_literal(
                    GraphemeCluster::merge(graphemes_second, graphemes_b),
                    config,
                );
                return Some(Expression::new_concatenation(
                    *first.clone(),
                    literal,
                    config,
                ));
            }
        }

        Some(Expression::new_concatenation(
            expr1.clone(),
            expr2.clone(),
            config,
        ))
    }

    fn union(
        a: &Option<Expression<'a>>,
        b: &Option<Expression<'a>>,
        config: &'a RegExpConfig,
    ) -> Option<Expression<'a>> {
        if let (Some(mut expr1), Some(mut expr2)) = (a.clone(), b.clone()) {
            if expr1 != expr2 {
                let common_prefix =
                    Self::remove_common_substring(&mut expr1, &mut expr2, Substring::Prefix);
                let common_suffix =
                    Self::remove_common_substring(&mut expr1, &mut expr2, Substring::Suffix);

                let mut result = if expr1.is_empty() {
                    Some(Expression::new_repetition(
                        expr2.clone(),
                        Quantifier::QuestionMark,
                        config,
                    ))
                } else if expr2.is_empty() {
                    Some(Expression::new_repetition(
                        expr1.clone(),
                        Quantifier::QuestionMark,
                        config,
                    ))
                } else {
                    None
                };

                if result.is_none() {
                    if let Expression::Repetition(expr, quantifier, _) = expr1.clone() {
                        if quantifier == Quantifier::QuestionMark {
                            let alternation =
                                Expression::new_alternation(*expr, expr2.clone(), config);
                            result = Some(Expression::new_repetition(
                                alternation,
                                Quantifier::QuestionMark,
                                config,
                            ));
                        }
                    }
                }

                if result.is_none() {
                    if let Expression::Repetition(expr, quantifier, _) = expr2.clone() {
                        if quantifier == Quantifier::QuestionMark {
                            let alternation =
                                Expression::new_alternation(expr1.clone(), *expr, config);
                            result = Some(Expression::new_repetition(
                                alternation,
                                Quantifier::QuestionMark,
                                config,
                            ));
                        }
                    }
                }

                if result.is_none() && expr1.is_single_codepoint() && expr2.is_single_codepoint() {
                    let first_char_set = Self::extract_character_set(expr1.clone());
                    let second_char_set = Self::extract_character_set(expr2.clone());
                    result = Some(Expression::new_character_class(
                        first_char_set,
                        second_char_set,
                        config,
                    ));
                }

                if result.is_none() {
                    result = Some(Expression::new_alternation(expr1, expr2, config));
                }

                if let Some(prefix) = common_prefix {
                    result = Some(Expression::new_concatenation(
                        Expression::new_literal(GraphemeCluster::from_graphemes(prefix), config),
                        result.unwrap(),
                        config,
                    ));
                }

                if let Some(suffix) = common_suffix {
                    result = Some(Expression::new_concatenation(
                        result.unwrap(),
                        Expression::new_literal(GraphemeCluster::from_graphemes(suffix), config),
                        config,
                    ));
                }

                result
            } else if a.is_some() {
                a.clone()
            } else if b.is_some() {
                b.clone()
            } else {
                None
            }
        } else if a.is_some() {
            a.clone()
        } else if b.is_some() {
            b.clone()
        } else {
            None
        }
    }

    fn flatten_alternations(
        flattened_options: &mut Vec<Expression<'a>>,
        current_options: Vec<Expression<'a>>,
    ) {
        for option in current_options {
            if let Expression::Alternation(expr_options, _) = option {
                Self::flatten_alternations(flattened_options, expr_options);
            } else {
                flattened_options.push(option);
            }
        }
    }

    fn extract_character_set(expr: Expression) -> BTreeSet<char> {
        match expr {
            Expression::Literal(cluster, _) => {
                let single_char = cluster
                    .graphemes()
                    .first()
                    .unwrap()
                    .value()
                    .chars()
                    .next()
                    .unwrap();
                btreeset![single_char]
            }
            Expression::CharacterClass(char_set, _) => char_set,
            _ => BTreeSet::new(),
        }
    }

    fn remove_common_substring(
        a: &mut Expression,
        b: &mut Expression,
        substring: Substring,
    ) -> Option<Vec<Grapheme>> {
        let common_substring = Self::find_common_substring(a, b, &substring);
        if let Some(value) = &common_substring {
            a.remove_substring(&substring, value.len());
            b.remove_substring(&substring, value.len());
        }
        common_substring
    }

    fn find_common_substring(
        a: &Expression,
        b: &Expression,
        substring: &Substring,
    ) -> Option<Vec<Grapheme>> {
        let mut graphemes_a = a.value(Some(substring)).unwrap_or_else(|| vec![]);
        let mut graphemes_b = b.value(Some(substring)).unwrap_or_else(|| vec![]);
        let mut common_graphemes = vec![];

        if let Substring::Suffix = substring {
            graphemes_a.reverse();
            graphemes_b.reverse();
        }

        for pair in graphemes_a.iter().zip_longest(graphemes_b.iter()) {
            match pair {
                Both(grapheme_a, grapheme_b) => {
                    if grapheme_a == grapheme_b {
                        common_graphemes.push(grapheme_a.clone());
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }

        if let Substring::Suffix = substring {
            common_graphemes.reverse();
        }

        if common_graphemes.is_empty() {
            None
        } else {
            Some(common_graphemes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_correct_string_representation_of_alternation_1() {
        let config = RegExpConfig::new();
        let literal1 = Expression::new_literal(GraphemeCluster::from("abc", &config), &config);
        let literal2 = Expression::new_literal(GraphemeCluster::from("def", &config), &config);
        let alternation = Expression::new_alternation(literal1, literal2, &config);
        assert_eq!(alternation.to_string(), "abc|def");
    }

    #[test]
    fn ensure_correct_string_representation_of_alternation_2() {
        let config = RegExpConfig::new();
        let literal1 = Expression::new_literal(GraphemeCluster::from("a", &config), &config);
        let literal2 = Expression::new_literal(GraphemeCluster::from("ab", &config), &config);
        let literal3 = Expression::new_literal(GraphemeCluster::from("abc", &config), &config);
        let alternation1 = Expression::new_alternation(literal1, literal2, &config);
        let alternation2 = Expression::new_alternation(alternation1, literal3, &config);
        assert_eq!(alternation2.to_string(), "abc|ab|a");
    }

    #[test]
    fn ensure_correct_string_representation_of_character_class_1() {
        let config = RegExpConfig::new();
        let char_class = Expression::new_character_class(btreeset!['a'], btreeset!['b'], &config);
        assert_eq!(char_class.to_string(), "[ab]");
    }

    #[test]
    fn ensure_correct_string_representation_of_character_class_2() {
        let config = RegExpConfig::new();
        let char_class =
            Expression::new_character_class(btreeset!['a', 'b'], btreeset!['c'], &config);
        assert_eq!(char_class.to_string(), "[a-c]");
    }

    #[test]
    fn ensure_correct_string_representation_of_concatenation_1() {
        let config = RegExpConfig::new();
        let literal1 = Expression::new_literal(GraphemeCluster::from("abc", &config), &config);
        let literal2 = Expression::new_literal(GraphemeCluster::from("def", &config), &config);
        let concatenation = Expression::new_concatenation(literal1, literal2, &config);
        assert_eq!(concatenation.to_string(), "abcdef");
    }

    #[test]
    fn ensure_correct_string_representation_of_concatenation_2() {
        let config = RegExpConfig::new();
        let literal1 = Expression::new_literal(GraphemeCluster::from("abc", &config), &config);
        let literal2 = Expression::new_literal(GraphemeCluster::from("def", &config), &config);
        let repetition = Expression::new_repetition(literal1, Quantifier::KleeneStar, &config);
        let concatenation = Expression::new_concatenation(repetition, literal2, &config);
        assert_eq!(concatenation.to_string(), "(?:abc)*def");
    }

    #[test]
    fn ensure_correct_removal_of_prefix_in_literal() {
        let config = RegExpConfig::new();
        let mut literal =
            Expression::new_literal(GraphemeCluster::from("abcdef", &config), &config);
        assert_eq!(
            literal.value(None),
            Some(
                vec!["a", "b", "c", "d", "e", "f"]
                    .iter()
                    .map(|&it| Grapheme::from(it, &config))
                    .collect_vec()
            )
        );

        literal.remove_substring(&Substring::Prefix, 2);
        assert_eq!(
            literal.value(None),
            Some(
                vec!["c", "d", "e", "f"]
                    .iter()
                    .map(|&it| Grapheme::from(it, &config))
                    .collect_vec()
            )
        );
    }

    #[test]
    fn ensure_correct_removal_of_suffix_in_literal() {
        let config = RegExpConfig::new();
        let mut literal =
            Expression::new_literal(GraphemeCluster::from("abcdef", &config), &config);
        assert_eq!(
            literal.value(None),
            Some(
                vec!["a", "b", "c", "d", "e", "f"]
                    .iter()
                    .map(|&it| Grapheme::from(it, &config))
                    .collect_vec()
            )
        );

        literal.remove_substring(&Substring::Suffix, 2);
        assert_eq!(
            literal.value(None),
            Some(
                vec!["a", "b", "c", "d"]
                    .iter()
                    .map(|&it| Grapheme::from(it, &config))
                    .collect_vec()
            )
        );
    }

    #[test]
    fn ensure_correct_string_representation_of_repetition_1() {
        let config = RegExpConfig::new();
        let literal = Expression::new_literal(GraphemeCluster::from("abc", &config), &config);
        let repetition = Expression::new_repetition(literal, Quantifier::KleeneStar, &config);
        assert_eq!(repetition.to_string(), "(?:abc)*");
    }

    #[test]
    fn ensure_correct_string_representation_of_repetition_2() {
        let config = RegExpConfig::new();
        let literal = Expression::new_literal(GraphemeCluster::from("a", &config), &config);
        let repetition = Expression::new_repetition(literal, Quantifier::QuestionMark, &config);
        assert_eq!(repetition.to_string(), "a?");
    }
}
