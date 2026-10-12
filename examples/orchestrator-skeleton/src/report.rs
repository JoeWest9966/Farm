//! 一致性套件 / 整链 tracer 共用的结果簿（ticket #26）。
//!
//! 每条断言是一条 [`Check`]（名字 + 过没过 + 机械细节）；[`Report`] 汇总并可判全绿。
//! 命名用 GLOSSARY 词汇，detail 带可核对的机械事实（sha、事件 kind、闭集码…）。

/// 一条机械断言。
#[derive(Clone, Debug)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

impl Check {
    pub fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), ok: true, detail: detail.into() }
    }
    pub fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), ok: false, detail: detail.into() }
    }
}

/// 一批断言 + 一个可选的整体标签。
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub label: String,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), checks: Vec::new() }
    }

    pub fn push(&mut self, check: Check) {
        self.checks.push(check);
    }

    /// 记录一条断言：`cond` 为真即 pass，否则 fail（detail 说明机械事实）。
    pub fn record(&mut self, name: impl Into<String>, cond: bool, detail: impl Into<String>) {
        self.push(if cond { Check::pass(name, detail) } else { Check::fail(name, detail) });
    }

    pub fn all_ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    pub fn failures(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| !c.ok).collect()
    }

    /// 人读的一屏报告（CI 日志 / 终端）。
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("== {} ==\n", self.label));
        for c in &self.checks {
            out.push_str(&format!(
                "[{}] {} — {}\n",
                if c.ok { "PASS" } else { "FAIL" },
                c.name,
                c.detail
            ));
        }
        let total = self.checks.len();
        let passed = total - self.failures().len();
        out.push_str(&format!("\n{passed}/{total} checks passed{}\n", if self.all_ok() { " (ALL GREEN)" } else { " — FAILURES ABOVE" }));
        out
    }
}
