//! 校验仓库内示例 Skill 的格式：发现、元数据与资源清单。
//!
//! 示例位于 `examples/skills`，与默认安装目录使用同一套 `SKILL.md` 格式。

use std::path::Path;

use agent::skill;

#[test]
fn example_skills_are_discoverable() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("examples").join("skills");

    let skills = skill::discover(&[root]);
    let names: Vec<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
    assert!(names.contains(&"haloeth-selftest"), "discovered: {names:?}");

    let selftest = skills
        .iter()
        .find(|skill| skill.name == "haloeth-selftest")
        .expect("the self test skill should be present");
    assert!(!selftest.description.is_empty(), "frontmatter description is required");
    assert!(selftest.when_to_use.contains("self test"));
    assert!(selftest.body.contains("final_answer"), "body should describe the closing step");

    let resources = selftest.resources();
    assert!(resources.iter().any(|name| name == "checklist.md"), "{resources:?}");
    assert!(resources.iter().any(|name| name == "report-template.md"), "{resources:?}");
    assert!(!resources.iter().any(|name| name == "SKILL.md"), "manifest is not a resource");
}
