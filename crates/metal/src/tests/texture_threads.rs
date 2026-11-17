use super::*;
use ash::vk;

#[test]
fn shared_texture_attachments_reuse_views_across_recording_threads() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let texture = resource::new_texture(
        gpu_of(device).unwrap(),
        resource::TextureDescriptor {
            texture_type: 3,
            pixel_format: 80,
            width: 16,
            height: 16,
            depth: 1,
            mipmap_level_count: 5,
            sample_count: 1,
            array_length: 4,
            storage_mode: 2,
            usage: gpu::USAGE_RENDER_TARGET,
        },
    );
    assert!(!texture.is_null());
    let barrier = std::sync::Barrier::new(8);
    let address = texture as usize;
    let views = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    crate::enter_darwin_thread();
                    // SAFETY: retained until every worker has joined; only the
                    // synchronized view cache changes during these calls.
                    let texture = unsafe { object::body_ref::<resource::Texture>(address as Id) };
                    let mut views = Vec::new();
                    for level in 0..5 {
                        for slice in 0..4 {
                            barrier.wait();
                            let view = texture.attachment_view(level, slice);
                            assert_ne!(view, vk::ImageView::null());
                            views.push(view);
                        }
                    }
                    views
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    for worker in &views[1..] {
        assert_eq!(worker, &views[0]);
    }
    let unique: std::collections::HashSet<_> = views[0].iter().collect();
    assert_eq!(unique.len(), 20);
    objc::arc::release(texture);
    objc::arc::release(device);
}
