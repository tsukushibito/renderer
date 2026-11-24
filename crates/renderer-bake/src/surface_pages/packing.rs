use super::{StaticReceiver, SurfaceLayout, SurfaceRegion};
use renderer::pbr::LightmapPage;
use std::{collections::HashSet, error::Error};

// All counts refer to resident static surface lighting.
#[derive(Default, Clone, Copy)]
pub(super) struct Metrics {
    pub bytes: u64,
    pub groups: usize,
    pub images: usize,
    pub physical_texels: u64,
}

#[derive(Clone, Copy)]
struct Block {
    receiver: usize,
    size: [u32; 2],
    // [0,0] denotes front and back on two dedicated physical pages.
    back: Option<[u32; 2]>,
}

#[derive(Clone, Copy)]
struct Space {
    page: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[derive(Default)]
struct Packer {
    pages: Vec<[u32; 2]>,
    free: Vec<Space>,
    shelf: Option<usize>,
    cursor: [u32; 2],
    row_height: u32,
}

impl Packer {
    fn new_page(&mut self, size: [u32; 2]) -> usize {
        let index = self.pages.len();
        self.pages.push(size);
        index
    }

    fn next_shelf(&mut self, [w, h]: [u32; 2], limit: u32) -> (usize, [u32; 2]) {
        if self.cursor[0] + w > limit {
            self.cursor = [0, self.cursor[1] + self.row_height];
            self.row_height = 0;
        }
        if self.cursor[1] + h > limit {
            self.shelf = None;
        }
        let page = match self.shelf {
            Some(page) => page,
            None => {
                let page = self.new_page([0, 0]);
                self.shelf = Some(page);
                self.cursor = [0, 0];
                self.row_height = 0;
                page
            }
        };
        let origin = self.cursor;
        self.cursor[0] += w;
        self.row_height = self.row_height.max(h);
        (page, origin)
    }

    // Guillotine packing reuses gaps in older rows/pages. Split into two
    // disjoint rectangles, keeping the larger leftover axis unfragmented.
    fn best_space(&mut self, [w, h]: [u32; 2], limit: u32, trim: bool) -> (usize, [u32; 2]) {
        let fit = self
            .free
            .iter()
            .enumerate()
            .filter(|(_, s)| w <= s.w && h <= s.h)
            .min_by_key(|(_, s)| {
                let [pw, ph] = self.pages[s.page];
                let growth = if trim {
                    pw.max(s.x + w) * ph.max(s.y + h) - pw * ph
                } else {
                    0
                };
                (growth, (s.w - w).min(s.h - h), (s.w - w).max(s.h - h))
            })
            .map(|(i, _)| i);
        let space = match fit {
            Some(i) => self.free.swap_remove(i),
            None => Space {
                page: self.new_page([0, 0]),
                x: 0,
                y: 0,
                w: limit,
                h: limit,
            },
        };
        let dw = space.w - w;
        let dh = space.h - h;
        if dw > 0 {
            self.free.push(Space {
                x: space.x + w,
                w: dw,
                h: if dw >= dh { space.h } else { h },
                ..space
            });
        }
        if dh > 0 {
            self.free.push(Space {
                y: space.y + h,
                h: dh,
                w: if dw >= dh { w } else { space.w },
                ..space
            });
        }
        (space.page, [space.x, space.y])
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    Shelf,
    BestFit,
    LeastGrowth,
}

fn candidate<'a>(
    receivers: &'a [StaticReceiver],
    order: &[Block],
    limit: u32,
    scenarios: usize,
    method: Method,
) -> Option<SurfaceLayout<'a>> {
    let mut packer = Packer::default();
    let mut regions = Vec::new();
    let mut members = vec![];
    let mut groups = HashSet::new();
    for block in order {
        let receiver = &receivers[block.receiver];
        let mut emit = |page, back, origin| {
            regions.push((
                block.receiver,
                SurfaceRegion {
                    placement: receiver.placement,
                    page,
                    back,
                    origin,
                    extent: receiver.extent,
                },
            ))
        };
        let (front, back) = if block.back == Some([0, 0]) {
            let front = packer.new_page(block.size);
            let back = packer.new_page(block.size);
            emit(front, false, [1, 1]);
            emit(back, true, [1, 1]);
            (front, back)
        } else {
            let (page, [x, y]) = match method {
                Method::Shelf => packer.next_shelf(block.size, limit),
                Method::BestFit | Method::LeastGrowth => {
                    packer.best_space(block.size, limit, method == Method::LeastGrowth)
                }
            };
            let dims = &mut packer.pages[page];
            dims[0] = dims[0].max(x + block.size[0]);
            dims[1] = dims[1].max(y + block.size[1]);
            emit(page, false, [x + 1, y + 1]);
            if let Some([dx, dy]) = block.back {
                emit(page, true, [x + dx + 1, y + dy + 1]);
            }
            (page, page)
        };
        members.resize(packer.pages.len(), (0usize, 0));
        members[front].0 += 1;
        members[front].1 = back;
        groups.insert((receiver.material, front));
    }
    let area = |page: usize| {
        packer.pages[page]
            .into_iter()
            .map(u64::from)
            .product::<u64>()
    };
    let mut metrics = Metrics {
        groups: groups.len(),
        physical_texels: (0..packer.pages.len()).map(area).sum(),
        ..Metrics::default()
    };
    for (front, (count, back)) in members.into_iter().enumerate().filter(|(_, (n, _))| *n > 0) {
        let texels = area(front) + if back == front { 0 } else { area(back) };
        let [w, h] = LightmapPage::packed_extent(count, texels, scenarios)?;
        metrics.bytes += u64::from(w) * u64::from(h) * 16;
        metrics.images += 1;
    }
    Some(SurfaceLayout {
        receivers,
        regions,
        pages: packer.pages,
        scenarios,
        metrics,
        page_limit: limit,
    })
}

pub(super) fn select(
    receivers: &[StaticReceiver],
    maximum: u32,
    scenarios: usize,
    budget: Option<u64>,
) -> Result<SurfaceLayout<'_>, Box<dyn Error>> {
    let minimum = receivers.iter().flat_map(|r| r.extent).max().unwrap_or(0) + 2;
    let mut choices = Vec::new();
    let mut smallest = u64::MAX;
    let mut baseline_bytes = u64::MAX;
    for limit in [64, 128, 256, 512, 768, 1024, 1536, 2048, 3072, 4096]
        .into_iter()
        .filter(|&n| n < maximum)
        .chain([maximum])
        .filter(|&n| n >= minimum)
    {
        for ordering in [Ordering::Height, Ordering::Area, Ordering::Material] {
            let order = blocks(receivers, limit, ordering);
            for method in [Method::Shelf, Method::BestFit, Method::LeastGrowth] {
                let Some(layout) = candidate(receivers, &order, limit, scenarios, method) else {
                    continue;
                };
                let m = layout.metrics;
                smallest = smallest.min(m.physical_texels);
                if budget.is_some_and(|b| m.physical_texels > b) {
                    continue;
                }
                if limit == maximum.min(2048)
                    && matches!(ordering, Ordering::Height)
                    && method == Method::Shelf
                {
                    baseline_bytes = m.bytes;
                }
                choices.push((m, limit, ordering, method));
            }
        }
    }
    if let Some(min_bytes) = choices.iter().map(|c| c.0.bytes).min() {
        // Prefer batching within 1% of the smallest footprint, capped at the
        // previous shelf layout when that layout satisfies the user's budget.
        let ceiling = (min_bytes + min_bytes / 100).min(baseline_bytes);
        let &(_, limit, ordering, method) = choices
            .iter()
            .filter(|c| c.0.bytes <= ceiling)
            .min_by_key(|(m, ..)| (m.groups, m.images, m.bytes, m.physical_texels))
            .expect("minimum-byte layout is eligible");
        let order = blocks(receivers, limit, ordering);
        return Ok(candidate(receivers, &order, limit, scenarios, method)
            .expect("repeating a feasible deterministic layout"));
    }
    if smallest == u64::MAX {
        return Err("no lightmap layout fits the resident image limits".into());
    }
    Err(format!(
        "lightmaps require at least {smallest} texels among evaluated layouts, budget is {}",
        budget.unwrap()
    )
    .into())
}

#[derive(Clone, Copy)]
enum Ordering {
    Height,
    Area,
    Material,
}

fn blocks(receivers: &[StaticReceiver], limit: u32, ordering: Ordering) -> Vec<Block> {
    let mut order = Vec::with_capacity(receivers.len());
    for (index, r) in receivers.iter().enumerate() {
        let [w, h] = r.extent.map(|v| v + 2);
        let (size, back) = if !r.double_sided {
            ([w, h], None)
        } else if 2 * w <= limit {
            ([2 * w, h], Some([w, 0]))
        } else if 2 * h <= limit {
            ([w, 2 * h], Some([0, h]))
        } else {
            ([w, h], Some([0, 0]))
        };
        order.push(Block {
            receiver: index,
            size,
            back,
        });
    }
    order.sort_unstable_by_key(|b| {
        (
            if matches!(ordering, Ordering::Material) {
                receivers[b.receiver].material
            } else {
                0
            },
            std::cmp::Reverse(if matches!(ordering, Ordering::Area) {
                b.size[0] * b.size[1]
            } else {
                0
            }),
            std::cmp::Reverse(b.size[1]),
            std::cmp::Reverse(b.size[0]),
            receivers[b.receiver].placement,
        )
    });
    order
}
