use std::collections::BTreeMap;

const DIR_COLOR: &str = "\x1b[01;34m";
const FILE_COLOR: &str = "\x1b[00m";
const RESET: &str = "\x1b[0m";

#[derive(Debug, Default)]
struct Node {
    children: BTreeMap<String, Node>,
}

impl Node {
    fn insert_path(&mut self, path: &str) {
        let mut node = self;
        for part in path.split('/').filter(|part| !part.is_empty()) {
            node = node.children.entry(part.to_string()).or_default();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct RenderedTree {
    lines: Vec<String>,
    directories: usize,
    files: usize,
}

impl RenderedTree {
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn summary(&self) -> String {
        format!(
            "{} {}, {} {}",
            self.directories,
            if self.directories == 1 {
                "directory"
            } else {
                "directories"
            },
            self.files,
            if self.files == 1 { "file" } else { "files" }
        )
    }
}

#[derive(Debug, Default)]
struct Counts {
    directories: usize,
    files: usize,
}

pub fn render_path_tree(paths: &[String], no_color: bool) -> RenderedTree {
    let mut root = Node::default();
    for path in paths {
        root.insert_path(path);
    }

    let mut lines = vec![format_entry(".", true, no_color)];
    render_children(&root, "", no_color, &mut lines);

    let counts = count_root(&root);
    RenderedTree {
        lines,
        directories: counts.directories,
        files: counts.files,
    }
}

fn render_children(node: &Node, prefix: &str, no_color: bool, lines: &mut Vec<String>) {
    let child_count = node.children.len();
    for (index, (name, child)) in node.children.iter().enumerate() {
        let is_last = index + 1 == child_count;
        let connector = if is_last { "└── " } else { "├── " };
        let is_directory = !child.children.is_empty();
        lines.push(format!(
            "{prefix}{connector}{}",
            format_entry(name, is_directory, no_color)
        ));

        if is_directory {
            let child_prefix = if is_last {
                format!("{prefix}    ")
            } else {
                format!("{prefix}│\u{a0}\u{a0} ")
            };
            render_children(child, &child_prefix, no_color, lines);
        }
    }
}

fn format_entry(name: &str, is_directory: bool, no_color: bool) -> String {
    if no_color {
        return name.to_string();
    }

    let color = if is_directory { DIR_COLOR } else { FILE_COLOR };
    format!("{color}{name}{RESET}")
}

fn count_root(root: &Node) -> Counts {
    let mut counts = Counts {
        directories: 1,
        files: 0,
    };
    for child in root.children.values() {
        counts.add(count_child(child));
    }
    counts
}

fn count_child(node: &Node) -> Counts {
    if node.children.is_empty() {
        return Counts {
            directories: 0,
            files: 1,
        };
    }

    let mut counts = Counts {
        directories: 1,
        files: 0,
    };
    for child in node.children.values() {
        counts.add(count_child(child));
    }
    counts
}

impl Counts {
    fn add(&mut self, other: Counts) {
        self.directories += other.directories;
        self.files += other.files;
    }
}

#[cfg(test)]
mod tests {
    use super::render_path_tree;

    #[test]
    fn renders_single_file_tree() {
        let files = vec!["README.md".to_string()];
        let tree = render_path_tree(&files, true);

        assert_eq!(
            tree.lines(),
            &[".".to_string(), "└── README.md".to_string()]
        );
        assert_eq!(tree.summary(), "1 directory, 1 file");
    }

    #[test]
    fn renders_nested_paths_sorted_and_counted() {
        let files = vec![
            "z.txt".to_string(),
            "a.txt".to_string(),
            "dir/b.txt".to_string(),
            "dir/a.txt".to_string(),
        ];
        let tree = render_path_tree(&files, true);

        assert_eq!(
            tree.lines(),
            &[
                ".".to_string(),
                "├── a.txt".to_string(),
                "├── dir".to_string(),
                "│\u{a0}\u{a0} ├── a.txt".to_string(),
                "│\u{a0}\u{a0} └── b.txt".to_string(),
                "└── z.txt".to_string(),
            ]
        );
        assert_eq!(tree.summary(), "2 directories, 4 files");
    }

    #[test]
    fn preserves_dot_prefixed_segments() {
        let files = vec![
            ".agents/skills/foo/SKILL.md".to_string(),
            ".github/workflows/ci.yml".to_string(),
        ];
        let tree = render_path_tree(&files, true);

        assert_eq!(
            tree.lines(),
            &[
                ".".to_string(),
                "├── .agents".to_string(),
                "│\u{a0}\u{a0} └── skills".to_string(),
                "│\u{a0}\u{a0}     └── foo".to_string(),
                "│\u{a0}\u{a0}         └── SKILL.md".to_string(),
                "└── .github".to_string(),
                "    └── workflows".to_string(),
                "        └── ci.yml".to_string(),
            ]
        );
        assert_eq!(tree.summary(), "6 directories, 2 files");
    }

    #[test]
    fn path_that_is_also_a_parent_renders_as_directory() {
        let files = vec!["src".to_string(), "src/lib.rs".to_string()];
        let tree = render_path_tree(&files, true);

        assert_eq!(
            tree.lines(),
            &[
                ".".to_string(),
                "└── src".to_string(),
                "    └── lib.rs".to_string(),
            ]
        );
        assert_eq!(tree.summary(), "2 directories, 1 file");
    }

    #[test]
    fn color_mode_matches_tree_style_entry_colors() {
        let files = vec!["src/lib.rs".to_string()];
        let tree = render_path_tree(&files, false);

        assert_eq!(tree.lines()[0], "\x1b[01;34m.\x1b[0m");
        assert_eq!(tree.lines()[1], "└── \x1b[01;34msrc\x1b[0m");
        assert_eq!(tree.lines()[2], "    └── \x1b[00mlib.rs\x1b[0m");
    }
}
