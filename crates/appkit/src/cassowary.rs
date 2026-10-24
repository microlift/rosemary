//! A Cassowary linear constraint solver: the incremental simplex method of
//! Badros, Borning and Stuckey, as Auto Layout itself uses.
//!
//! Required constraints must hold; every other one is an error term in the
//! objective, weighted by its strength, which the solver minimises. Variables
//! are plain indices the caller hands out.

use std::collections::BTreeMap;

/// A caller's variable.
pub type Var = usize;

/// A linear expression: `sum(coefficient * variable) + constant`.
#[derive(Clone, Debug, Default)]
pub struct Expression {
    pub terms: Vec<(Var, f64)>,
    pub constant: f64,
}

impl Expression {
    pub fn constant(value: f64) -> Expression {
        Expression {
            terms: Vec::new(),
            constant: value,
        }
    }

    pub fn var(var: Var) -> Expression {
        Expression {
            terms: vec![(var, 1.0)],
            constant: 0.0,
        }
    }

    pub fn plus(mut self, other: &Expression, scale: f64) -> Expression {
        self.terms
            .extend(other.terms.iter().map(|(v, c)| (*v, c * scale)));
        self.constant += other.constant * scale;
        self
    }

    pub fn add_constant(mut self, value: f64) -> Expression {
        self.constant += value;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    LessOrEqual,
    Equal,
    GreaterOrEqual,
}

/// The strength of a constraint that must hold.
pub const REQUIRED: f64 = f64::INFINITY;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Kind {
    External,
    Slack,
    Error,
    Dummy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Symbol {
    id: u32,
    kind: Kind,
}

const EPSILON: f64 = 1e-8;

fn near_zero(value: f64) -> bool {
    value.abs() < EPSILON
}

#[derive(Clone, Debug, Default)]
struct Row {
    constant: f64,
    cells: BTreeMap<Symbol, f64>,
}

impl Row {
    fn new(constant: f64) -> Row {
        Row {
            constant,
            cells: BTreeMap::new(),
        }
    }

    fn insert_symbol(&mut self, symbol: Symbol, coefficient: f64) {
        let entry = self.cells.entry(symbol).or_insert(0.0);
        *entry += coefficient;
        if near_zero(*entry) {
            self.cells.remove(&symbol);
        }
    }

    fn insert_row(&mut self, other: &Row, coefficient: f64) {
        self.constant += other.constant * coefficient;
        for (symbol, c) in &other.cells {
            self.insert_symbol(*symbol, c * coefficient);
        }
    }

    fn reverse_sign(&mut self) {
        self.constant = -self.constant;
        for c in self.cells.values_mut() {
            *c = -*c;
        }
    }

    /// Solve `0 = row` for `symbol`, which the row then defines.
    fn solve_for(&mut self, symbol: Symbol) {
        let coefficient = -1.0
            / self
                .cells
                .remove(&symbol)
                .expect("the symbol is in the row");
        self.constant *= coefficient;
        for c in self.cells.values_mut() {
            *c *= coefficient;
        }
    }

    /// Solve `lhs = row` for `rhs`.
    fn solve_for_pair(&mut self, lhs: Symbol, rhs: Symbol) {
        self.insert_symbol(lhs, -1.0);
        self.solve_for(rhs);
    }

    fn coefficient_for(&self, symbol: Symbol) -> f64 {
        self.cells.get(&symbol).copied().unwrap_or(0.0)
    }

    fn substitute(&mut self, symbol: Symbol, row: &Row) {
        if let Some(c) = self.cells.remove(&symbol) {
            self.insert_row(row, c);
        }
    }
}

/// Why a required constraint could not be added.
#[derive(Debug, PartialEq, Eq)]
pub struct Unsatisfiable;

#[derive(Default)]
pub struct Solver {
    rows: BTreeMap<Symbol, Row>,
    vars: BTreeMap<Var, Symbol>,
    objective: Row,
    artificial: Option<Row>,
    next: u32,
}

/// Which row [`Solver::optimize`] minimises.
#[derive(Clone, Copy)]
enum Goal {
    Objective,
    Artificial,
}

impl Solver {
    pub fn new() -> Solver {
        Solver::default()
    }

    fn symbol(&mut self, kind: Kind) -> Symbol {
        self.next += 1;
        Symbol {
            id: self.next,
            kind,
        }
    }

    fn var_symbol(&mut self, var: Var) -> Symbol {
        if let Some(symbol) = self.vars.get(&var) {
            return *symbol;
        }
        let symbol = self.symbol(Kind::External);
        self.vars.insert(var, symbol);
        symbol
    }

    /// The value the solution gives `var`; zero for one never mentioned.
    pub fn value(&self, var: Var) -> f64 {
        self.vars
            .get(&var)
            .and_then(|symbol| self.rows.get(symbol))
            .map_or(0.0, |row| row.constant)
    }

    /// Add `expression relation 0` at `strength`. A required one that
    /// contradicts the required ones before it is refused and changes
    /// nothing; any other always goes in.
    pub fn add(
        &mut self,
        expression: &Expression,
        relation: Relation,
        strength: f64,
    ) -> Result<(), Unsatisfiable> {
        let (row, marker, other) = self.create_row(expression, relation, strength);
        let mut row = row;
        let mut subject = choose_subject(&row, marker, other);
        if subject.is_none() && row.cells.keys().all(|s| s.kind == Kind::Dummy) {
            if !near_zero(row.constant) {
                return Err(Unsatisfiable);
            }
            subject = Some(marker);
        }
        match subject {
            None => {
                if !self.add_with_artificial(row) {
                    return Err(Unsatisfiable);
                }
            }
            Some(subject) => {
                row.solve_for(subject);
                self.substitute(subject, &row);
                self.rows.insert(subject, row);
            }
        }
        self.optimize(Goal::Objective);
        Ok(())
    }

    fn create_row(
        &mut self,
        expression: &Expression,
        relation: Relation,
        strength: f64,
    ) -> (Row, Symbol, Option<Symbol>) {
        let mut row = Row::new(expression.constant);
        for (var, coefficient) in &expression.terms {
            if near_zero(*coefficient) {
                continue;
            }
            let symbol = self.var_symbol(*var);
            match self.rows.get(&symbol) {
                Some(basic) => {
                    let basic = basic.clone();
                    row.insert_row(&basic, *coefficient);
                }
                None => row.insert_symbol(symbol, *coefficient),
            }
        }
        let required = strength.is_infinite();
        let (marker, other) = match relation {
            Relation::LessOrEqual | Relation::GreaterOrEqual => {
                let coefficient = match relation {
                    Relation::LessOrEqual => 1.0,
                    _ => -1.0,
                };
                let slack = self.symbol(Kind::Slack);
                row.insert_symbol(slack, coefficient);
                let other = (!required).then(|| {
                    let error = self.symbol(Kind::Error);
                    row.insert_symbol(error, -coefficient);
                    self.objective.insert_symbol(error, strength);
                    error
                });
                (slack, other)
            }
            Relation::Equal if required => {
                let dummy = self.symbol(Kind::Dummy);
                row.insert_symbol(dummy, 1.0);
                (dummy, None)
            }
            Relation::Equal => {
                let plus = self.symbol(Kind::Error);
                let minus = self.symbol(Kind::Error);
                row.insert_symbol(plus, -1.0);
                row.insert_symbol(minus, 1.0);
                self.objective.insert_symbol(plus, strength);
                self.objective.insert_symbol(minus, strength);
                (plus, Some(minus))
            }
        };
        if row.constant < 0.0 {
            row.reverse_sign();
        }
        (row, marker, other)
    }

    fn add_with_artificial(&mut self, row: Row) -> bool {
        let artificial = self.symbol(Kind::Slack);
        self.rows.insert(artificial, row.clone());
        self.artificial = Some(row);
        self.optimize(Goal::Artificial);
        let success = self
            .artificial
            .take()
            .is_some_and(|a| near_zero(a.constant));
        if let Some(mut row) = self.rows.remove(&artificial) {
            if row.cells.is_empty() {
                return success;
            }
            let Some(entering) = row
                .cells
                .keys()
                .find(|s| matches!(s.kind, Kind::Slack | Kind::Error))
                .copied()
            else {
                return false;
            };
            row.solve_for_pair(artificial, entering);
            self.substitute(entering, &row);
            self.rows.insert(entering, row);
        }
        for row in self.rows.values_mut() {
            row.cells.remove(&artificial);
        }
        self.objective.cells.remove(&artificial);
        success
    }

    fn substitute(&mut self, symbol: Symbol, row: &Row) {
        // A row left infeasible here is what dual optimisation repairs after
        // an edit; there are no edit variables, so none arises.
        for r in self.rows.values_mut() {
            r.substitute(symbol, row);
        }
        self.objective.substitute(symbol, row);
        if let Some(artificial) = self.artificial.as_mut() {
            artificial.substitute(symbol, row);
        }
    }

    fn goal(&self, goal: Goal) -> &Row {
        match goal {
            Goal::Objective => &self.objective,
            Goal::Artificial => self
                .artificial
                .as_ref()
                .expect("optimising the artificial row"),
        }
    }

    fn optimize(&mut self, goal: Goal) {
        // Bounded: Bland's rule is not used, so a degenerate cycle must not
        // hang a layout pass.
        for _ in 0..10_000 {
            let Some(entering) = self
                .goal(goal)
                .cells
                .iter()
                .find(|(s, c)| s.kind != Kind::Dummy && **c < 0.0)
                .map(|(s, _)| *s)
            else {
                return;
            };
            let mut leaving = None;
            let mut ratio = f64::INFINITY;
            for (symbol, row) in &self.rows {
                if symbol.kind == Kind::External {
                    continue;
                }
                let c = row.coefficient_for(entering);
                if c < 0.0 {
                    let r = -row.constant / c;
                    if r < ratio {
                        ratio = r;
                        leaving = Some(*symbol);
                    }
                }
            }
            let Some(leaving) = leaving else {
                // Unbounded: the objective has no minimum, which only a
                // malformed system has.
                return;
            };
            let mut row = self.rows.remove(&leaving).expect("found among the rows");
            row.solve_for_pair(leaving, entering);
            self.substitute(entering, &row);
            self.rows.insert(entering, row);
        }
    }
}

fn choose_subject(row: &Row, marker: Symbol, other: Option<Symbol>) -> Option<Symbol> {
    if let Some(external) = row.cells.keys().find(|s| s.kind == Kind::External) {
        return Some(*external);
    }
    for symbol in std::iter::once(marker).chain(other) {
        if matches!(symbol.kind, Kind::Slack | Kind::Error) && row.coefficient_for(symbol) < 0.0 {
            return Some(symbol);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eq(solver: &mut Solver, lhs: Expression, rhs: Expression, strength: f64) {
        solver
            .add(&lhs.plus(&rhs, -1.0), Relation::Equal, strength)
            .unwrap();
    }

    #[test]
    fn required_equalities_are_solved() {
        let mut s = Solver::new();
        // x + 10 = y, y = 50.
        eq(
            &mut s,
            Expression::var(0).add_constant(10.0),
            Expression::var(1),
            REQUIRED,
        );
        eq(
            &mut s,
            Expression::var(1),
            Expression::constant(50.0),
            REQUIRED,
        );
        assert!((s.value(0) - 40.0).abs() < 1e-6);
        assert!((s.value(1) - 50.0).abs() < 1e-6);
    }

    #[test]
    fn a_stronger_preference_wins() {
        let mut s = Solver::new();
        eq(
            &mut s,
            Expression::var(0),
            Expression::constant(100.0),
            10.0,
        );
        eq(
            &mut s,
            Expression::var(0),
            Expression::constant(200.0),
            1000.0,
        );
        assert!((s.value(0) - 200.0).abs() < 1e-6);
    }

    #[test]
    fn inequalities_bound_a_preference() {
        let mut s = Solver::new();
        // x >= 30 required, x = 10 preferred.
        s.add(
            &Expression::var(0).add_constant(-30.0),
            Relation::GreaterOrEqual,
            REQUIRED,
        )
        .unwrap();
        eq(&mut s, Expression::var(0), Expression::constant(10.0), 1.0);
        assert!((s.value(0) - 30.0).abs() < 1e-6);
    }

    #[test]
    fn contradictory_requirements_are_refused() {
        let mut s = Solver::new();
        eq(
            &mut s,
            Expression::var(0),
            Expression::constant(1.0),
            REQUIRED,
        );
        let refused = s.add(
            &Expression::var(0).add_constant(-2.0),
            Relation::Equal,
            REQUIRED,
        );
        assert_eq!(refused, Err(Unsatisfiable));
        assert!((s.value(0) - 1.0).abs() < 1e-6);
    }

    /// Two views side by side in a 300-point container, 8 apart, with the
    /// left one's width preferred at 100: the right one takes the rest.
    #[test]
    fn a_row_of_views_fills_its_container() {
        let mut s = Solver::new();
        let (l1, w1, l2, w2) = (0, 1, 2, 3);
        eq(
            &mut s,
            Expression::var(l1),
            Expression::constant(0.0),
            REQUIRED,
        );
        eq(
            &mut s,
            Expression::var(l2),
            Expression::var(l1)
                .plus(&Expression::var(w1), 1.0)
                .add_constant(8.0),
            REQUIRED,
        );
        eq(
            &mut s,
            Expression::var(l2).plus(&Expression::var(w2), 1.0),
            Expression::constant(300.0),
            REQUIRED,
        );
        eq(
            &mut s,
            Expression::var(w1),
            Expression::constant(100.0),
            500.0,
        );
        eq(&mut s, Expression::var(w2), Expression::constant(10.0), 1.0);
        assert!((s.value(w1) - 100.0).abs() < 1e-6);
        assert!((s.value(l2) - 108.0).abs() < 1e-6);
        assert!((s.value(w2) - 192.0).abs() < 1e-6);
    }
}
