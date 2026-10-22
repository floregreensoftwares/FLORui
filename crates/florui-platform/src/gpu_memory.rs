//! How a window's graphics device takes memory from the system. Every window has a device of its
//! own, and the allocator under it works in blocks: large ones mean few requests while the window
//! draws and uploads a lot, small ones a small footprint for a window that draws little.

/// How a window's graphics device takes memory from the system.
///
/// A window that makes no choice (`WindowOptions::gpu_memory`) gets [`GpuMemory::default`]. The
/// blocks are those of the allocators of DirectX 12 and Vulkan; other backends ignore the choice.
/// Each window pays it once, so an application with two windows holds twice the blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuMemory {
    /// Blocks of 128 MiB of video memory and 64 MiB beside it, growing to 256 and 128: the
    /// allocator rarely goes back to the system, which matters to a window that creates many
    /// graphics resources.
    #[default]
    Performance,
    /// Blocks of 8 MiB and 4 MiB, growing to 64 and 32. For a window that draws little.
    Compact,
    /// Blocks of `min_block_mib` growing to `max_block_mib` of video memory, and half of that
    /// beside it. Both are kept between 4 and 256, and the maximum is never below the minimum.
    Manual {
        min_block_mib: u32,
        max_block_mib: u32,
    },
}

const MIB: u64 = 1024 * 1024;

impl GpuMemory {
    /// What `FLORUI_GPU_MEMORY` asks for (`performance`, `compact`, or a range of MiB such as
    /// `16-64`), over whatever a window chose: a way to compare the choices without changing
    /// code. A value that means nothing is reported and ignored.
    pub fn from_environment() -> Option<Self> {
        let value = std::env::var("FLORUI_GPU_MEMORY").ok()?;
        let parsed = Self::parse(&value);
        if parsed.is_none() {
            eprintln!(
                "florui-platform: FLORUI_GPU_MEMORY={value:?} is not performance, compact or a \
                 range of MiB such as 16-64; ignored"
            );
        }
        parsed
    }

    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("performance") {
            return Some(Self::Performance);
        }
        if text.eq_ignore_ascii_case("compact") {
            return Some(Self::Compact);
        }
        let (min, max) = text.split_once('-')?;
        let min_block_mib = min.trim().parse().ok()?;
        let max_block_mib = max.trim().parse().ok()?;
        (min_block_mib <= max_block_mib).then_some(Self::Manual {
            min_block_mib,
            max_block_mib,
        })
    }

    /// The choice that holds for a window made by the desktop host: the environment's, else the
    /// window's own.
    pub(crate) fn resolve(window_choice: Self) -> Self {
        Self::prefer(Self::from_environment(), window_choice)
    }

    fn prefer(environment: Option<Self>, window_choice: Self) -> Self {
        environment.unwrap_or(window_choice)
    }

    pub(crate) fn hints(self) -> wgpu::MemoryHints {
        match self {
            Self::Performance => wgpu::MemoryHints::Performance,
            Self::Compact => wgpu::MemoryHints::MemoryUsage,
            Self::Manual {
                min_block_mib,
                max_block_mib,
            } => {
                let min = u64::from(min_block_mib).clamp(4, 256);
                let max = u64::from(max_block_mib).clamp(min, 256);
                wgpu::MemoryHints::Manual {
                    suballocated_device_memory_block_size: min * MIB..max * MIB,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_that_chooses_nothing_gets_the_performance_blocks() {
        assert_eq!(GpuMemory::default(), GpuMemory::Performance);
    }

    #[test]
    fn the_environment_names_a_choice_or_a_range() {
        assert_eq!(GpuMemory::parse("Compact"), Some(GpuMemory::Compact));
        assert_eq!(
            GpuMemory::parse(" performance "),
            Some(GpuMemory::Performance)
        );
        assert_eq!(
            GpuMemory::parse("16-64"),
            Some(GpuMemory::Manual {
                min_block_mib: 16,
                max_block_mib: 64
            })
        );
    }

    #[test]
    fn the_environment_wins_over_the_window_and_its_absence_leaves_the_window_be() {
        let (compact, performance) = (GpuMemory::Compact, GpuMemory::Performance);
        assert_eq!(GpuMemory::prefer(Some(compact), performance), compact);
        assert_eq!(GpuMemory::prefer(None, compact), compact);
    }

    #[test]
    fn what_the_environment_cannot_mean_is_ignored() {
        for text in ["", "fast", "64-16", "-", "8-", "a-b", "8:64"] {
            assert_eq!(GpuMemory::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn each_choice_becomes_the_matching_hint_of_the_graphics_library() {
        assert!(matches!(
            GpuMemory::Performance.hints(),
            wgpu::MemoryHints::Performance
        ));
        assert!(matches!(
            GpuMemory::Compact.hints(),
            wgpu::MemoryHints::MemoryUsage
        ));
        assert_eq!(blocks(16, 64), 16 * MIB..64 * MIB);
    }

    fn blocks(min_block_mib: u32, max_block_mib: u32) -> std::ops::Range<u64> {
        let wgpu::MemoryHints::Manual {
            suballocated_device_memory_block_size,
        } = (GpuMemory::Manual {
            min_block_mib,
            max_block_mib,
        })
        .hints()
        else {
            panic!("a range asks for manual blocks");
        };
        suballocated_device_memory_block_size
    }

    #[test]
    fn a_range_built_in_code_is_brought_inside_what_the_allocator_keeps() {
        assert_eq!(blocks(0, 512), 4 * MIB..256 * MIB);
        assert_eq!(blocks(64, 16), 64 * MIB..64 * MIB);
    }
}
