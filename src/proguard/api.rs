//! Request and response types for `/deobfuscate/java/v1`, and the code which
//! applies a mapping file to them.
//!
//! Request:
//!
//! ```json
//! {
//!   "jobs": [
//!     {
//!       "mappingFile": { "uuid": "fe506e08-58e3-3f15-9117-67ccb4d01f19" },
//!       "stacks": [
//!         {
//!           "exception": "a.b.C",
//!           "frames": [
//!             { "class": "a.b.d", "method": "a", "line": 12, "file": "SourceFile" }
//!           ]
//!         }
//!       ],
//!       "classes": ["a.b.e"]
//!     }
//!   ]
//! }
//! ```
//!
//! Each job names one mapping file by its ProGuard UUID. The UUID must be in
//! the lowercase dashed form shown above. Frames in a stack are ordered from the
//! innermost frame to the outermost, like in a Java stack trace. This order
//! matters because R8 outlining and frame rewrite rules depend on adjacent
//! frames and on the exception class. `exception`, `line`, `file` and
//! `classes` are optional. Without a line number, inlined frames can't be
//! recovered and overloaded methods can't be told apart.
//!
//! Response:
//!
//! ```json
//! {
//!   "results": [
//!     {
//!       "mappingFile": { "found": true },
//!       "stacks": [
//!         {
//!           "exception": "org.example.SomeException",
//!           "frames": [
//!             [
//!               { "class": "org.example.Inlined", "method": "inner", "file": "Inlined.kt", "line": 21 },
//!               { "class": "org.example.Outer", "method": "outer", "file": "Outer.kt", "line": 469 }
//!             ]
//!           ]
//!         }
//!       ],
//!       "classes": { "a.b.e": "org.example.SomeClass" }
//!     }
//!   ]
//! }
//! ```
//!
//! Each input frame produces one array of output frames, innermost first. It
//! has more than one entry if R8 inlined functions into the frame. It is empty
//! if the frame should be dropped, for example because it is an R8 outline
//! frame. If the frame can't be deobfuscated, it contains the input frame
//! unchanged. If the mapping file can't be found, `mappingFile` has
//! `"found": false` and an `error` string, and all frames are returned
//! unchanged. `classes` only contains the classes which were found.

use std::collections::BTreeMap;

use proguard::{class_name_to_descriptor, ProguardCache, StackFrame};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub jobs: Vec<Job>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Job {
    pub mapping_file: MappingFileRequest,
    #[serde(default)]
    pub stacks: Vec<Stack>,
    #[serde(default)]
    pub classes: Vec<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingFileRequest {
    pub uuid: String,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Stack {
    /// The class of the exception which was thrown by the innermost frame.
    pub exception: Option<String>,
    pub frames: Vec<Frame>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub class: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Serialize, Debug)]
pub struct Response {
    pub results: Vec<JobResult>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JobResult {
    pub mapping_file: MappingFileStatus,
    pub stacks: Vec<StackResult>,
    pub classes: BTreeMap<String, String>,
}

#[derive(Serialize, Debug)]
pub struct MappingFileStatus {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct StackResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception: Option<String>,
    pub frames: Vec<Vec<Frame>>,
}

/// Builds the result for a job whose mapping file couldn't be loaded. All
/// frames are returned unchanged.
pub fn unresolved_job_result(job: Job, error: String) -> JobResult {
    let stacks = job
        .stacks
        .into_iter()
        .map(|stack| StackResult {
            exception: stack.exception,
            frames: stack.frames.into_iter().map(|frame| vec![frame]).collect(),
        })
        .collect();
    JobResult {
        mapping_file: MappingFileStatus {
            found: false,
            error: Some(error),
        },
        stacks,
        classes: BTreeMap::new(),
    }
}

/// Deobfuscates all stacks and classes of a job using `cache`.
pub fn resolve_job(job: &Job, cache: &ProguardCache) -> JobResult {
    let stacks = job
        .stacks
        .iter()
        .map(|stack| resolve_stack(stack, cache))
        .collect();
    let classes = job
        .classes
        .iter()
        .filter_map(|class| Some((class.clone(), cache.remap_class(class)?.to_owned())))
        .collect();
    JobResult {
        mapping_file: MappingFileStatus {
            found: true,
            error: None,
        },
        stacks,
        classes,
    }
}

fn resolve_stack(stack: &Stack, cache: &ProguardCache) -> StackResult {
    let exception = stack.exception.as_ref().map(|class| {
        cache
            .remap_class(class)
            .map(str::to_owned)
            .unwrap_or_else(|| class.clone())
    });
    // Rewrite rules can remove frames from the innermost frame, depending on
    // the type of the thrown exception.
    let exception_descriptor = exception.as_deref().map(class_name_to_descriptor);
    let mut apply_rewrite = exception_descriptor.is_some();
    let mut carried_outline_pos = None;

    let frames = stack
        .frames
        .iter()
        .map(|frame| {
            // The proguard crate uses line 0 for "no line".
            let line = frame.line.unwrap_or(0) as usize;
            let input = match &frame.file {
                Some(file) => StackFrame::with_file(&frame.class, &frame.method, line, file),
                None => StackFrame::new(&frame.class, &frame.method, line),
            };
            let Some(iter) = cache.remap_frame_with_context(
                &input,
                exception_descriptor.as_deref(),
                apply_rewrite,
                &mut carried_outline_pos,
            ) else {
                // This is an outline frame. Its position has been carried
                // over to the next frame.
                return vec![];
            };
            apply_rewrite = false;

            let had_mappings = iter.had_mappings();
            let remapped: Vec<Frame> = iter.map(|f| convert_frame(&f)).collect();
            if remapped.is_empty() && !had_mappings {
                // Nothing is known about this frame.
                return vec![frame.clone()];
            }
            // If the frame had mappings and the result is empty, a rewrite
            // rule removed it.
            remapped
        })
        .collect();

    StackResult { exception, frames }
}

fn convert_frame(frame: &StackFrame) -> Frame {
    Frame {
        class: frame.class().to_owned(),
        method: frame.method().to_owned(),
        file: frame.file().map(str::to_owned),
        line: frame
            .line()
            .filter(|line| *line != 0)
            .and_then(|line| u32::try_from(line).ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proguard::ProguardMapping;

    const MAPPING: &[u8] = br#"# compiler: R8
org.example.Outer -> a.a:
# {"id":"sourceFile","fileName":"Outer.kt"}
    1:5:void outer():469:469 -> a
    6:10:double org.example.Inlined.inner():21:21 -> a
    6:10:void outer():470 -> a
org.example.SomeException -> a.b:
# {"id":"sourceFile","fileName":"SomeException.kt"}
"#;

    fn with_cache(f: impl FnOnce(&ProguardCache)) {
        let mapping = ProguardMapping::new(MAPPING);
        let mut buf = Vec::new();
        ProguardCache::write(&mapping, &mut buf).unwrap();
        f(&ProguardCache::parse(&buf).unwrap());
    }

    fn frame(class: &str, method: &str, file: Option<&str>, line: Option<u32>) -> Frame {
        Frame {
            class: class.into(),
            method: method.into(),
            file: file.map(Into::into),
            line,
        }
    }

    fn job(stacks: Vec<Stack>, classes: Vec<&str>) -> Job {
        Job {
            mapping_file: MappingFileRequest {
                uuid: "00000000-0000-0000-0000-000000000000".into(),
            },
            stacks,
            classes: classes.into_iter().map(Into::into).collect(),
        }
    }

    #[test]
    fn expands_inlined_frames() {
        with_cache(|cache| {
            let job = job(
                vec![Stack {
                    exception: Some("a.b".into()),
                    frames: vec![
                        frame("a.a", "a", Some("SourceFile"), Some(7)),
                        frame("a.a", "a", None, Some(2)),
                        frame("java.lang.Thread", "run", Some("Thread.java"), Some(1012)),
                    ],
                }],
                vec!["a.b", "x.y"],
            );
            let result = resolve_job(&job, cache);
            assert!(result.mapping_file.found);
            let stack = &result.stacks[0];
            assert_eq!(
                stack.exception.as_deref(),
                Some("org.example.SomeException")
            );
            assert_eq!(
                stack.frames,
                vec![
                    vec![
                        frame("org.example.Inlined", "inner", Some("Inlined.kt"), Some(21)),
                        frame("org.example.Outer", "outer", Some("Outer.kt"), Some(470)),
                    ],
                    vec![frame(
                        "org.example.Outer",
                        "outer",
                        Some("Outer.kt"),
                        Some(469)
                    )],
                    vec![frame(
                        "java.lang.Thread",
                        "run",
                        Some("Thread.java"),
                        Some(1012)
                    )],
                ]
            );
            assert_eq!(
                result.classes,
                BTreeMap::from([("a.b".into(), "org.example.SomeException".into())])
            );
        });
    }

    #[test]
    fn frame_without_line_resolves_to_outer_method() {
        with_cache(|cache| {
            let job = job(
                vec![Stack {
                    exception: None,
                    frames: vec![frame("a.a", "a", None, None)],
                }],
                vec![],
            );
            let result = resolve_job(&job, cache);
            assert_eq!(
                result.stacks[0].frames,
                vec![vec![frame(
                    "org.example.Outer",
                    "outer",
                    Some("Outer.kt"),
                    None
                )]]
            );
        });
    }

    #[test]
    fn unresolved_job_returns_frames_unchanged() {
        let input = frame("a.a", "a", None, Some(3));
        let job = job(
            vec![Stack {
                exception: Some("a.b".into()),
                frames: vec![input.clone()],
            }],
            vec!["a.b"],
        );
        let result = unresolved_job_result(job, "not found".into());
        assert!(!result.mapping_file.found);
        assert_eq!(result.mapping_file.error.as_deref(), Some("not found"));
        assert_eq!(result.stacks[0].exception.as_deref(), Some("a.b"));
        assert_eq!(result.stacks[0].frames, vec![vec![input]]);
        assert!(result.classes.is_empty());
    }
}
