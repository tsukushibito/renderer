//! Allocation-free observation of VMA callbacks, forwarding directly to System.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::c_void;

use ash::vk;

use crate::measurement::{HostEvent, NativeEvent, record_native, record_vma_host};

#[derive(Clone, Copy)]
struct Header {
    layout: Layout,
    prefix: usize,
    size: usize,
}

// The header immediately precedes the aligned user pointer, so free never
// needs a lookup table or another allocation to recover the original Layout.
unsafe fn allocate_bytes(size: usize, alignment: usize) -> *mut c_void {
    let Ok(user) = Layout::from_size_align(size.max(1), alignment) else {
        return std::ptr::null_mut();
    };
    let Ok((layout, prefix)) = Layout::new::<Header>().extend(user) else {
        return std::ptr::null_mut();
    };
    let layout = layout.pad_to_align();
    // SAFETY: Layout construction checked size/alignment. Use System directly
    // so VMA metadata is not also charged to an application's CountingAllocator.
    let base = unsafe { System.alloc(layout) };
    if base.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: extend reserved the aligned user bytes after at least one Header.
    // Both its address and the immediately preceding Header are aligned; the
    // latter fits entirely inside the live allocation.
    unsafe {
        let pointer = base.add(prefix);
        pointer
            .sub(size_of::<Header>())
            .cast::<Header>()
            .write(Header {
                layout,
                prefix,
                size,
            });
        pointer.cast()
    }
}

unsafe fn header(pointer: *mut c_void) -> Header {
    // SAFETY: Every non-null callback pointer was returned by allocate_bytes,
    // remains live, and has its initialized aligned Header directly before it.
    unsafe {
        pointer
            .cast::<u8>()
            .sub(size_of::<Header>())
            .cast::<Header>()
            .read()
    }
}

unsafe fn free_bytes(pointer: *mut c_void) {
    if pointer.is_null() {
        return;
    }
    // SAFETY: Callback ownership supplies a live pointer from allocate_bytes.
    let header = unsafe { header(pointer) };
    // SAFETY: Recover exactly the System base and Layout retained by allocation.
    unsafe { System.dealloc(pointer.cast::<u8>().sub(header.prefix), header.layout) };
}

unsafe extern "system" fn allocate(
    _: *mut c_void,
    size: usize,
    alignment: usize,
    _: vk::SystemAllocationScope,
) -> *mut c_void {
    record_vma_host(HostEvent::Allocate, size);
    // SAFETY: The callback creates a new owned allocation for the native caller.
    unsafe { allocate_bytes(size, alignment) }
}

unsafe extern "system" fn reallocate(
    _: *mut c_void,
    original: *mut c_void,
    size: usize,
    alignment: usize,
    _: vk::SystemAllocationScope,
) -> *mut c_void {
    record_vma_host(HostEvent::Reallocate, size);
    if size == 0 {
        // SAFETY: Native realloc relinquishes this live pointer when size is zero.
        unsafe { free_bytes(original) };
        return std::ptr::null_mut();
    }
    // SAFETY: Create a distinct allocation; failure leaves original untouched.
    let replacement = unsafe { allocate_bytes(size, alignment) };
    if replacement.is_null() || original.is_null() {
        return replacement;
    }
    // SAFETY: The original belongs to this callback and remains live until the
    // successful replacement has copied its initialized prefix. Blocks differ.
    unsafe {
        let old = header(original);
        std::ptr::copy_nonoverlapping(
            original.cast::<u8>(),
            replacement.cast::<u8>(),
            old.size.min(size),
        );
        free_bytes(original);
    }
    replacement
}

unsafe extern "system" fn free(_: *mut c_void, pointer: *mut c_void) {
    record_vma_host(HostEvent::Deallocate, 0);
    // SAFETY: Native caller relinquishes a callback allocation, or passes null.
    unsafe { free_bytes(pointer) };
}

pub(super) fn host_callbacks() -> vk::AllocationCallbacks<'static> {
    vk::AllocationCallbacks::default()
        .pfn_allocation(Some(allocate))
        .pfn_reallocation(Some(reallocate))
        .pfn_free(Some(free))
}

// Generic opaque-pointer arguments let the compiler infer vk-mem's private
// VmaAllocator_T type from the public create-info field without an ABI cast.
// VMA calls allocation after successful vkAllocateMemory, and free before the
// vkFreeMemory call. No callback dereferences the allocator or user pointer.
pub(super) unsafe extern "C" fn device_allocate<T>(
    _: *mut T,
    _: u32,
    _: vk::DeviceMemory,
    _: vk::DeviceSize,
    _: *mut c_void,
) {
    record_native(NativeEvent::MemoryAllocate, 1);
}

pub(super) unsafe extern "C" fn device_free<T>(
    _: *mut T,
    _: u32,
    _: vk::DeviceMemory,
    _: vk::DeviceSize,
    _: *mut c_void,
) {
    record_native(NativeEvent::MemoryFree, 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_reallocation_preserves_alignment_contents_and_failure_ownership() {
        // SAFETY: This test owns every callback allocation and never accesses it
        // after free or successful realloc. Tested lengths are inside each block.
        unsafe {
            let first = allocate(
                std::ptr::null_mut(),
                17,
                256,
                vk::SystemAllocationScope::OBJECT,
            );
            assert!(!first.is_null());
            assert_eq!(first as usize % 256, 0);
            std::ptr::write_bytes(first.cast::<u8>(), 0x5a, 17);
            let failed = reallocate(
                std::ptr::null_mut(),
                first,
                usize::MAX,
                256,
                vk::SystemAllocationScope::OBJECT,
            );
            assert!(failed.is_null());
            assert_eq!(
                std::slice::from_raw_parts(first.cast::<u8>(), 17),
                &[0x5a; 17]
            );
            let second = reallocate(
                std::ptr::null_mut(),
                first,
                33,
                256,
                vk::SystemAllocationScope::OBJECT,
            );
            assert!(!second.is_null());
            assert_eq!(second as usize % 256, 0);
            assert_eq!(
                std::slice::from_raw_parts(second.cast::<u8>(), 17),
                &[0x5a; 17]
            );
            assert!(
                reallocate(
                    std::ptr::null_mut(),
                    second,
                    0,
                    256,
                    vk::SystemAllocationScope::OBJECT
                )
                .is_null()
            );
            free(std::ptr::null_mut(), std::ptr::null_mut());
        }
    }
}
