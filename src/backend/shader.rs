use super::device::{Device, SamplerDesc};
use crate::chunky_list::TempList;
use ash::{version::DeviceV1_0, vk};
use byte_slice_cast::AsSliceOf as _;
use derive_builder::Builder;
use std::{
    collections::{hash_map::Entry, HashMap},
    ffi::CString,
    sync::Arc,
};

type StageDescriptorSetLayout = HashMap<u32, HashMap<u32, rspirv_reflect::DescriptorInfo>>;

pub fn create_descriptor_set_layouts(
    device: &Device,
    descriptor_sets: StageDescriptorSetLayout,
    stage_flags: vk::ShaderStageFlags,
    set_flags: &[(usize, vk::DescriptorSetLayoutCreateFlags)],
) -> Vec<vk::DescriptorSetLayout> {
    let samplers = TempList::new();

    descriptor_sets
        .into_iter()
        .map(|(set_index, set)| {
            let mut bindings: Vec<vk::DescriptorSetLayoutBinding> = Vec::with_capacity(set.len());

            for (binding_index, binding) in set.into_iter() {
                match binding.ty {
                    rspirv_reflect::DescriptorType::UNIFORM_BUFFER
                    | rspirv_reflect::DescriptorType::STORAGE_IMAGE
                    | rspirv_reflect::DescriptorType::STORAGE_BUFFER => bindings.push(
                        vk::DescriptorSetLayoutBinding::builder()
                            .binding(binding_index)
                            //.descriptor_count(binding.count)
                            .descriptor_count(1) // TODO
                            .descriptor_type(match binding.ty {
                                rspirv_reflect::DescriptorType::UNIFORM_BUFFER => {
                                    vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC
                                }
                                rspirv_reflect::DescriptorType::STORAGE_IMAGE => {
                                    vk::DescriptorType::STORAGE_IMAGE
                                }
                                rspirv_reflect::DescriptorType::STORAGE_BUFFER => {
                                    vk::DescriptorType::STORAGE_BUFFER
                                }
                                _ => unimplemented!("{:?}", binding),
                            })
                            .stage_flags(stage_flags)
                            .build(),
                    ),
                    rspirv_reflect::DescriptorType::SAMPLED_IMAGE => {
                        bindings.push(
                            vk::DescriptorSetLayoutBinding::builder()
                                .binding(binding_index)
                                //.descriptor_count(binding.count)
                                .descriptor_count(1) // TODO
                                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                                .stage_flags(stage_flags)
                                .build(),
                        );
                    }
                    rspirv_reflect::DescriptorType::SAMPLER => {
                        let name_prefix = "sampler_";
                        if let Some(mut spec) = binding.name.strip_prefix(name_prefix) {
                            let texel_filter = match &spec[..1] {
                                "n" => vk::Filter::NEAREST,
                                "l" => vk::Filter::LINEAR,
                                _ => panic!("{}", &spec[..1]),
                            };
                            spec = &spec[1..];

                            let mipmap_mode = match &spec[..1] {
                                "n" => vk::SamplerMipmapMode::NEAREST,
                                "l" => vk::SamplerMipmapMode::LINEAR,
                                _ => panic!("{}", &spec[..1]),
                            };
                            spec = &spec[1..];

                            let address_modes = match spec {
                                "r" => vk::SamplerAddressMode::REPEAT,
                                "mr" => vk::SamplerAddressMode::MIRRORED_REPEAT,
                                "c" => vk::SamplerAddressMode::CLAMP_TO_EDGE,
                                "cb" => vk::SamplerAddressMode::CLAMP_TO_BORDER,
                                _ => panic!("{}", spec),
                            };

                            bindings.push(
                                vk::DescriptorSetLayoutBinding::builder()
                                    //.descriptor_count(binding.count)
                                    .descriptor_count(1) // TODO
                                    .descriptor_type(vk::DescriptorType::SAMPLER)
                                    .stage_flags(stage_flags)
                                    .binding(binding_index)
                                    .immutable_samplers(std::slice::from_ref(samplers.add(
                                        device.get_sampler(SamplerDesc {
                                            texel_filter,
                                            mipmap_mode,
                                            address_modes,
                                        }),
                                    )))
                                    .build(),
                            );
                        } else {
                            panic!("{}", binding.name);
                        }
                    }

                    _ => unimplemented!("{:?}", binding),
                }
            }

            let flags = set_flags
                .iter()
                .find(|item| item.0 == set_index as usize)
                .map(|flags| flags.1)
                .unwrap_or_default();

            unsafe {
                device
                    .raw
                    .create_descriptor_set_layout(
                        &vk::DescriptorSetLayoutCreateInfo::builder()
                            .flags(flags)
                            .bindings(&bindings)
                            .build(),
                        None,
                    )
                    .unwrap()
            }
        })
        .collect()
}

#[derive(Builder)]
#[builder(pattern = "owned")]
pub struct ComputePipelineDesc<'a, 'b> {
    pub spirv: &'a [u8],
    pub entry_name: &'b str,
    #[builder(setter(strip_option), default)]
    pub descriptor_set_layout_flags: Option<&'a [(usize, vk::DescriptorSetLayoutCreateFlags)]>,
    #[builder(default)]
    pub push_constants_bytes: usize,
}

impl<'a, 'b> ComputePipelineDesc<'a, 'b> {
    pub fn builder() -> ComputePipelineDescBuilder<'a, 'b> {
        ComputePipelineDescBuilder::default()
    }
}

pub struct ComputePipeline {
    pub pipeline_layout: vk::PipelineLayout,
    pub pipeline: vk::Pipeline,
}

pub fn create_compute_pipeline(device: &Device, desc: ComputePipelineDesc) -> ComputePipeline {
    let descriptor_set_layouts = super::shader::create_descriptor_set_layouts(
        device,
        rspirv_reflect::Reflection::new_from_spirv(desc.spirv)
            .unwrap()
            .get_descriptor_sets()
            .unwrap(),
        vk::ShaderStageFlags::COMPUTE,
        desc.descriptor_set_layout_flags.unwrap_or(&[]),
    );

    let mut layout_create_info =
        vk::PipelineLayoutCreateInfo::builder().set_layouts(&descriptor_set_layouts);

    let push_constant_ranges = vk::PushConstantRange {
        stage_flags: vk::ShaderStageFlags::COMPUTE,
        offset: 0,
        size: desc.push_constants_bytes as _,
    };

    if desc.push_constants_bytes > 0 {
        layout_create_info =
            layout_create_info.push_constant_ranges(std::slice::from_ref(&push_constant_ranges));
    }

    unsafe {
        let shader_module = device
            .raw
            .create_shader_module(
                &vk::ShaderModuleCreateInfo::builder()
                    .code(desc.spirv.as_slice_of::<u32>().unwrap()),
                None,
            )
            .unwrap();

        let entry_name = CString::new(desc.entry_name).unwrap();
        let stage_create_info = vk::PipelineShaderStageCreateInfo::builder()
            .module(shader_module)
            .stage(vk::ShaderStageFlags::COMPUTE)
            .name(&entry_name);

        let pipeline_layout = device
            .raw
            .create_pipeline_layout(&layout_create_info, None)
            .unwrap();

        let pipeline_info = vk::ComputePipelineCreateInfo::builder()
            .stage(stage_create_info.build())
            .layout(pipeline_layout);

        let pipeline = device
            .raw
            // TODO: pipeline cache
            .create_compute_pipelines(vk::PipelineCache::null(), &[pipeline_info.build()], None)
            .expect("pipeline")[0];

        ComputePipeline {
            pipeline_layout,
            pipeline,
        }
    }
}

#[derive(Copy, Clone, Hash, Eq, PartialEq, Debug)]
pub enum RasterStage {
    Vertex,
    Pixel,
}

#[derive(Builder)]
#[builder(pattern = "owned")]
pub struct RasterShaderDesc<'a, 'b> {
    pub stage: RasterStage,
    pub spirv: &'a [u8],
    pub entry_name: &'b str,
    #[builder(setter(strip_option), default)]
    pub descriptor_set_layout_flags: Option<&'a [(usize, vk::DescriptorSetLayoutCreateFlags)]>,
    #[builder(default)]
    pub push_constants_bytes: usize,
}

impl<'a, 'b> RasterShaderDesc<'a, 'b> {
    pub fn new(
        stage: RasterStage,
        spirv: &'a [u8],
        entry_name: &'b str,
    ) -> RasterShaderDescBuilder<'a, 'b> {
        RasterShaderDescBuilder::default()
            .stage(stage)
            .spirv(spirv)
            .entry_name(entry_name)
    }
}

//#[derive(Builder)]
//#[builder(pattern = "owned")]
pub struct RasterPipelineDesc<'a, 'b> {
    pub shaders: &'a [RasterShaderDesc<'a, 'b>],
    pub render_pass: Arc<RenderPass>,
}

pub struct RasterPipeline {
    pub pipeline_layout: vk::PipelineLayout,
    pub pipeline: vk::Pipeline,
    pub render_pass: Arc<RenderPass>,
    //pub framebuffer: vk::Framebuffer,
}

pub struct RenderPassAttachmentDesc {
    pub format: vk::Format,
    pub load_op: vk::AttachmentLoadOp,
    pub store_op: vk::AttachmentStoreOp,
    pub samples: vk::SampleCountFlags,
}

#[allow(dead_code)]
impl RenderPassAttachmentDesc {
    pub fn new(format: vk::Format) -> Self {
        Self {
            format,
            load_op: vk::AttachmentLoadOp::LOAD,
            store_op: vk::AttachmentStoreOp::STORE,
            samples: vk::SampleCountFlags::TYPE_1,
        }
    }

    pub fn garbage_input(mut self) -> Self {
        self.load_op = vk::AttachmentLoadOp::DONT_CARE;
        self
    }

    pub fn clear_input(mut self) -> Self {
        self.load_op = vk::AttachmentLoadOp::CLEAR;
        self
    }

    pub fn discard_output(mut self) -> Self {
        self.store_op = vk::AttachmentStoreOp::DONT_CARE;
        self
    }

    fn to_vk(
        &self,
        initial_layout: vk::ImageLayout,
        final_layout: vk::ImageLayout,
    ) -> vk::AttachmentDescription {
        vk::AttachmentDescription {
            format: self.format,
            samples: self.samples,
            load_op: self.load_op,
            store_op: self.store_op,
            initial_layout,
            final_layout,
            ..Default::default()
        }
    }
}

pub struct RenderPassDesc<'a> {
    pub color_attachments: &'a [RenderPassAttachmentDesc],
    pub depth_attachment: Option<RenderPassAttachmentDesc>,
}

pub struct RenderPass {
    pub raw: vk::RenderPass,
}

pub fn create_render_pass(
    device: &Device,
    desc: RenderPassDesc<'_>,
) -> anyhow::Result<Arc<RenderPass>> {
    let renderpass_attachments = desc
        .color_attachments
        .iter()
        .map(|a| {
            a.to_vk(
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            )
        })
        .chain(desc.depth_attachment.as_ref().map(|a| {
            a.to_vk(
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            )
        }))
        .collect::<Vec<_>>();

    let color_attachment_refs = (0..desc.color_attachments.len() as u32)
        .map(|attachment| vk::AttachmentReference {
            attachment,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        })
        .collect::<Vec<_>>();

    let depth_attachment_ref = vk::AttachmentReference {
        attachment: desc.color_attachments.len() as u32,
        layout: vk::ImageLayout::DEPTH_ATTACHMENT_STENCIL_READ_ONLY_OPTIMAL,
    };

    // TODO: Calculate optimal dependencies. using implicit dependencies for now.
    /*let dependencies = [vk::SubpassDependency {
        src_subpass: vk::SUBPASS_EXTERNAL,
        src_stage_mask: vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
            | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS
            | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_READ
            | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
        dst_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        ..Default::default()
    }];*/

    let mut subpass_description = vk::SubpassDescription::builder()
        .color_attachments(&color_attachment_refs)
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS);

    if desc.depth_attachment.is_some() {
        subpass_description = subpass_description.depth_stencil_attachment(&depth_attachment_ref);
    }
    let subpass_description = subpass_description.build();

    let subpasses = [subpass_description];
    let render_pass_create_info = vk::RenderPassCreateInfo::builder()
        .attachments(&renderpass_attachments)
        .subpasses(&subpasses);

    unsafe {
        Ok(Arc::new(RenderPass {
            raw: device
                .raw
                .create_render_pass(&render_pass_create_info, None)
                .unwrap(),
        }))
    }
}

pub fn create_raster_pipeline(
    device: &Device,
    desc: RasterPipelineDesc,
) -> anyhow::Result<RasterPipeline> {
    let stage_layouts = desc
        .shaders
        .iter()
        .map(|desc| {
            rspirv_reflect::Reflection::new_from_spirv(desc.spirv)
                .unwrap()
                .get_descriptor_sets()
                .unwrap()
        })
        .collect::<Vec<_>>();

    let descriptor_set_layouts = super::shader::create_descriptor_set_layouts(
        device,
        merge_shader_stage_layouts(stage_layouts),
        vk::ShaderStageFlags::ALL_GRAPHICS,
        //desc.descriptor_set_layout_flags.unwrap_or(&[]),  // TODO: merge flags
        &[],
    );

    unsafe {
        let layout_create_info = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&descriptor_set_layouts)
            .build();

        let pipeline_layout = device
            .raw
            .create_pipeline_layout(&layout_create_info, None)
            .unwrap();

        let entry_names = TempList::new();
        let shader_stage_create_infos: Vec<_> = desc
            .shaders
            .iter()
            .map(|desc| {
                let shader_info = vk::ShaderModuleCreateInfo::builder()
                    .code(desc.spirv.as_slice_of::<u32>().unwrap());

                let shader_module = device
                    .raw
                    .create_shader_module(&shader_info, None)
                    .expect("Shader module error");

                let stage = match desc.stage {
                    RasterStage::Vertex => vk::ShaderStageFlags::VERTEX,
                    RasterStage::Pixel => vk::ShaderStageFlags::FRAGMENT,
                };

                vk::PipelineShaderStageCreateInfo::builder()
                    .module(shader_module)
                    .name(entry_names.add(CString::new(desc.entry_name).unwrap()))
                    .stage(stage)
                    .build()
            })
            .collect();

        let vertex_input_state_info = vk::PipelineVertexInputStateCreateInfo {
            vertex_attribute_description_count: 0,
            p_vertex_attribute_descriptions: std::ptr::null(),
            vertex_binding_description_count: 0,
            p_vertex_binding_descriptions: std::ptr::null(),
            ..Default::default()
        };
        let vertex_input_assembly_state_info = vk::PipelineInputAssemblyStateCreateInfo {
            topology: vk::PrimitiveTopology::TRIANGLE_LIST,
            ..Default::default()
        };

        let viewport_state_info = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1);

        let rasterization_info = vk::PipelineRasterizationStateCreateInfo {
            front_face: vk::FrontFace::COUNTER_CLOCKWISE,
            line_width: 1.0,
            polygon_mode: vk::PolygonMode::FILL,
            /*cull_mode: if opts.face_cull {
                ash::vk::CullModeFlags::BACK
            } else {
                ash::vk::CullModeFlags::NONE
            },*/
            cull_mode: ash::vk::CullModeFlags::NONE,
            ..Default::default()
        };
        let multisample_state_info = vk::PipelineMultisampleStateCreateInfo {
            rasterization_samples: vk::SampleCountFlags::TYPE_1,
            ..Default::default()
        };
        let noop_stencil_state = vk::StencilOpState {
            fail_op: vk::StencilOp::KEEP,
            pass_op: vk::StencilOp::KEEP,
            depth_fail_op: vk::StencilOp::KEEP,
            compare_op: vk::CompareOp::ALWAYS,
            ..Default::default()
        };
        let depth_state_info = vk::PipelineDepthStencilStateCreateInfo {
            depth_test_enable: 1,
            depth_write_enable: 1,
            depth_compare_op: vk::CompareOp::GREATER_OR_EQUAL,
            front: noop_stencil_state,
            back: noop_stencil_state,
            max_depth_bounds: 1.0,
            ..Default::default()
        };
        let color_blend_attachment_states = [vk::PipelineColorBlendAttachmentState {
            blend_enable: 0,
            src_color_blend_factor: vk::BlendFactor::SRC_COLOR,
            dst_color_blend_factor: vk::BlendFactor::ONE_MINUS_DST_COLOR,
            color_blend_op: vk::BlendOp::ADD,
            src_alpha_blend_factor: vk::BlendFactor::ZERO,
            dst_alpha_blend_factor: vk::BlendFactor::ZERO,
            alpha_blend_op: vk::BlendOp::ADD,
            color_write_mask: vk::ColorComponentFlags::all(),
        }];
        let color_blend_state = vk::PipelineColorBlendStateCreateInfo::builder()
            .attachments(&color_blend_attachment_states);

        let dynamic_state = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic_state_info =
            vk::PipelineDynamicStateCreateInfo::builder().dynamic_states(&dynamic_state);

        let graphic_pipeline_info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&shader_stage_create_infos)
            .vertex_input_state(&vertex_input_state_info)
            .input_assembly_state(&vertex_input_assembly_state_info)
            .viewport_state(&viewport_state_info)
            .rasterization_state(&rasterization_info)
            .multisample_state(&multisample_state_info)
            .depth_stencil_state(&depth_state_info)
            .color_blend_state(&color_blend_state)
            .dynamic_state(&dynamic_state_info)
            .layout(pipeline_layout)
            .render_pass(desc.render_pass.raw);

        let pipeline = device
            .raw
            .create_graphics_pipelines(
                vk::PipelineCache::null(),
                &[graphic_pipeline_info.build()],
                None,
            )
            .expect("Unable to create graphics pipeline")[0];

        Ok(RasterPipeline {
            pipeline_layout,
            pipeline,
            render_pass: desc.render_pass.clone(),
            //framebuffer,
        })
    }
}

fn merge_shader_stage_layout_pair(
    src: StageDescriptorSetLayout,
    dst: &mut StageDescriptorSetLayout,
) {
    for (set_idx, set) in src.into_iter() {
        match dst.entry(set_idx) {
            Entry::Occupied(mut existing) => {
                let existing = existing.get_mut();
                for (binding_idx, binding) in set {
                    match existing.entry(binding_idx) {
                        Entry::Occupied(existing) => {
                            let existing = existing.get();
                            assert!(existing.ty == binding.ty);
                            assert!(existing.name == binding.name);
                        }
                        Entry::Vacant(vacant) => {
                            vacant.insert(binding);
                        }
                    }
                }
            }
            Entry::Vacant(vacant) => {
                vacant.insert(set);
            }
        }
    }
}

fn merge_shader_stage_layouts(stages: Vec<StageDescriptorSetLayout>) -> StageDescriptorSetLayout {
    let mut stages = stages.into_iter();
    let mut result = stages.next().unwrap_or_default();

    for stage in stages {
        merge_shader_stage_layout_pair(stage, &mut result);
    }

    result
}
