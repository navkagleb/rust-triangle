use crate::*;

const ASYNC_COMPUTE_FRAME_COUNT: u64 = 3;

pub fn start_thread(
    device: Arc<ID3D12Device4>,
    loaded_mesh_receiver: Receiver<Result<mesh::LoadedMesh>>,
    ready_mesh_sender: Sender<GpuMesh>,
) -> std::thread::JoinHandle<Result<()>> {
    std::thread::Builder::new()
        .name("async-compute".to_string())
        .spawn(move || -> Result<()> { async_compute_routine(&device, loaded_mesh_receiver, ready_mesh_sender) })
        .unwrap()
}

fn async_compute_routine(
    device: &ID3D12Device4,
    loaded_mesh_receiver: Receiver<Result<LoadedMesh>>,
    ready_mesh_sender: Sender<GpuMesh>,
) -> Result<()> {
    unsafe {
        let cmd_queue = device.CreateCommandQueue::<ID3D12CommandQueue>(&D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_COMPUTE,
            Priority: D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        })?;

        let mut fence_value = 0;
        let fence = device.CreateFence::<ID3D12Fence>(fence_value, D3D12_FENCE_FLAG_NONE)?;
        let fence_event = CreateEventA(None, false, false, s!("compute_fence_event"))?;

        let cmd_list_type = D3D12_COMMAND_LIST_TYPE_COMPUTE;
        let cmd_allocators: [_; ASYNC_COMPUTE_FRAME_COUNT as usize] = std::array::from_fn(|_| {
            device
                .CreateCommandAllocator::<ID3D12CommandAllocator>(cmd_list_type)
                .unwrap()
        });
        let cmd_list =
            device.CreateCommandList1::<ID3D12GraphicsCommandList1>(0, cmd_list_type, D3D12_COMMAND_LIST_FLAG_NONE)?;

        let mut pending_release = Vec::new();
        let mut pending_gpu_meshes = Vec::new();
        let mut batch_count = 0;

        loop {
            fence_value += 1;

            let mut batch = Vec::new();

            if pending_gpu_meshes.is_empty() {
                match loaded_mesh_receiver.recv() {
                    Ok(first) => batch.push(first?),
                    Err(_) => break,
                }
            }

            while let Ok(loaded_mesh) = loaded_mesh_receiver.try_recv() {
                batch.push(loaded_mesh?);
            }

            if !batch.is_empty() {
                batch_count += 1;

                let mut total_vertices_size = 0;
                let mut total_indices_size = 0;
                for LoadedMesh(mesh, _) in &batch {
                    total_vertices_size += size_of_val(mesh.vertices.as_slice());
                    total_indices_size += size_of_val(mesh.indices.as_slice());
                }

                let upload_buf = ID3D12Resource::new_buf(
                    &device,
                    D3D12_HEAP_TYPE_UPLOAD,
                    total_vertices_size + total_indices_size,
                )?;

                let mut mapped_ptr = std::ptr::null_mut::<std::ffi::c_void>();
                upload_buf.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }), Some(&mut mapped_ptr))?;

                let mapped_ptr = mapped_ptr as *mut u8;
                let mut upload_buf_offset = 0;

                let batch_len = batch.len();
                let cmd_allocator = &cmd_allocators[fence_value as usize % ASYNC_COMPUTE_FRAME_COUNT as usize];

                cmd_allocator.Reset()?;
                cmd_list.Reset(cmd_allocator, None)?;

                for LoadedMesh(mesh, gpu_mesh) in batch {
                    let vertices_size = size_of_val(mesh.vertices.as_slice());
                    let indices_size = size_of_val(mesh.indices.as_slice());

                    std::ptr::copy_nonoverlapping(
                        mesh.vertices.as_ptr(),
                        mapped_ptr.add(upload_buf_offset) as *mut MeshVertex,
                        mesh.vertices.len(),
                    );

                    std::ptr::copy_nonoverlapping(
                        mesh.indices.as_ptr(),
                        mapped_ptr.add(upload_buf_offset + vertices_size) as *mut u32,
                        mesh.indices.len(),
                    );

                    cmd_list.ResourceBarrier(&[
                        D3D12_RESOURCE_BARRIER::new_transition(
                            &gpu_mesh.vertex_buffer,
                            D3D12_RESOURCE_STATE_COMMON,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                        ),
                        D3D12_RESOURCE_BARRIER::new_transition(
                            &gpu_mesh.index_buffer,
                            D3D12_RESOURCE_STATE_COMMON,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                        ),
                    ]);

                    cmd_list.CopyBufferRegion(
                        &gpu_mesh.vertex_buffer,
                        0,
                        &upload_buf,
                        upload_buf_offset as u64,
                        vertices_size as u64,
                    );
                    upload_buf_offset += vertices_size;

                    cmd_list.CopyBufferRegion(
                        &gpu_mesh.index_buffer,
                        0,
                        &upload_buf,
                        upload_buf_offset as u64,
                        indices_size as u64,
                    );
                    upload_buf_offset += indices_size;

                    pending_gpu_meshes.push((fence_value, gpu_mesh));
                }

                upload_buf.Unmap(
                    0,
                    Some(&D3D12_RANGE {
                        Begin: 0,
                        End: total_vertices_size + total_indices_size,
                    }),
                );

                pending_release.push((fence_value, upload_buf.cast::<ID3D12Object>()?));

                cmd_list.Close()?;
                cmd_queue.ExecuteCommandLists(&[Some(cmd_list.cast::<ID3D12CommandList>()?)]);
                cmd_queue.Signal(&fence, fence_value)?;

                println!(
                    "[{:6}][AT] batch #{} (x{})",
                    GetCurrentProcessId(),
                    batch_count - 1,
                    batch_len
                );
            }

            if fence_value - fence.GetCompletedValue() >= ASYNC_COMPUTE_FRAME_COUNT as u64 {
                let fence_value_to_wait = fence_value - ASYNC_COMPUTE_FRAME_COUNT as u64 + 1;
                wait_for_gpu(&fence, fence_event, fence_value_to_wait)?;
            }

            let completed_fence_value = fence.GetCompletedValue();

            pending_release.retain(|(fence_value, _)| *fence_value > completed_fence_value);

            let mut i = 0;
            while i < pending_gpu_meshes.len() {
                if pending_gpu_meshes[i].0 > completed_fence_value {
                    i += 1;
                    continue;
                }

                let (_, gpu_mesh) = pending_gpu_meshes.swap_remove(i);
                ready_mesh_sender.send(gpu_mesh).unwrap();
            }
        }

        Ok(())
    }
}
