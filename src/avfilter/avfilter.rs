use std::{
    ffi::CStr,
    mem::size_of,
    ops::Drop,
    ptr::{self, NonNull},
};

use crate::{
    avutil::AVFrame,
    error::{Result, RsmpegError},
    ffi,
    shared::*,
};

wrap!(AVFilter: ffi::AVFilter);

impl AVFilter {
    pub fn get_by_name(filter_name: &CStr) -> Result<Self> {
        let filter = unsafe { ffi::avfilter_get_by_name(filter_name.as_ptr()) }
            .upgrade()
            .ok_or(RsmpegError::FilterNotFound)?;
        Ok(unsafe { Self::from_raw(filter) })
    }
}

impl Drop for AVFilter {
    fn drop(&mut self) {
        // Do nothing, filter is always static
    }
}

wrap_mut!(AVFilterContext: ffi::AVFilterContext);

impl AVFilterContext {
    pub fn set_property<U>(&mut self, key: &CStr, value: &U) -> Result<()> {
        unsafe {
            ffi::av_opt_set_bin(
                self.as_mut_ptr().cast(),
                key.as_ptr(),
                value as *const _ as *const u8,
                size_of::<U>() as i32,
                ffi::AV_OPT_SEARCH_CHILDREN as i32,
            )
        }
        .upgrade()
        .map_err(|_| RsmpegError::SetPropertyError)?;
        Ok(())
    }

    pub fn buffersrc_add_frame_flags(
        &mut self,
        mut frame: Option<AVFrame>,
        flags: i32,
    ) -> Result<()> {
        let frame_ptr = match frame.as_mut() {
            Some(frame) => frame.as_mut_ptr(),
            None => ptr::null_mut(),
        };
        unsafe { ffi::av_buffersrc_add_frame_flags(self.as_mut_ptr(), frame_ptr, flags) }
            .upgrade()
            .map_err(|_| RsmpegError::BufferSrcAddFrameError)?;
        Ok(())
    }

    pub fn buffersink_get_frame(&mut self) -> Result<AVFrame> {
        let mut frame = AVFrame::new();
        match unsafe { ffi::av_buffersink_get_frame(self.as_mut_ptr(), frame.as_mut_ptr()) }
            .upgrade()
        {
            Ok(_) => Ok(frame),
            Err(AVERROR_EAGAIN) => Err(RsmpegError::BufferSinkDrainError),
            Err(ffi::AVERROR_EOF) => Err(RsmpegError::BufferSinkEofError),
            Err(_) => Err(RsmpegError::BufferSinkGetFrameError),
        }
    }
}

wrap!(AVFilterInOut: ffi::AVFilterInOut);

impl AVFilterInOut {
    // This borrow may be too strict? May need redesign to be useable while ensuring safety.
    pub fn new(name: &CStr, filter_context: &mut AVFilterContext) -> Self {
        let name = unsafe { ffi::av_strdup(name.as_ptr()) }.upgrade().unwrap();
        let inout_ptr = unsafe { ffi::avfilter_inout_alloc() }.upgrade().unwrap();

        let inout_mut = unsafe { inout_ptr.as_ptr().as_mut().unwrap() };
        inout_mut.name = name.as_ptr();
        inout_mut.filter_ctx = filter_context.as_mut_ptr();
        inout_mut.pad_idx = 0;
        inout_mut.next = ptr::null_mut();

        unsafe { Self::from_raw(inout_ptr) }
    }
}

impl Drop for AVFilterInOut {
    fn drop(&mut self) {
        let mut inout = self.as_mut_ptr();
        unsafe {
            ffi::avfilter_inout_free(&mut inout);
        }
    }
}

wrap!(AVFilterGraph: ffi::AVFilterGraph);

impl AVFilterGraph {
    pub fn new() -> Self {
        let filter_graph = unsafe { ffi::avfilter_graph_alloc() }.upgrade().unwrap();

        unsafe { Self::from_raw(filter_graph) }
    }

    /// Add a graph described by a string to a graph.
    ///
    /// This function returns the inputs and outputs (if any) that are left
    /// unlinked after parsing the graph and the caller then deals with them.
    pub fn parse_ptr(
        &self,
        filter_spec: &CStr,
        mut inputs: AVFilterInOut,
        mut outputs: AVFilterInOut,
    ) -> Result<(Option<AVFilterInOut>, Option<AVFilterInOut>)> {
        let mut inputs_new = inputs.as_mut_ptr();
        let mut outputs_new = outputs.as_mut_ptr();

        // FFmpeg `avfilter_graph_parse*`'s documentation states:
        //
        // This function makes no reference whatsoever to already existing parts
        // of the graph and the inputs parameter will on return contain inputs
        // of the newly parsed part of the graph.  Analogously the outputs
        // parameter will contain outputs of the newly created filters.
        //
        // So the function is designed to take immutable reference to the FilterGraph
        unsafe {
            ffi::avfilter_graph_parse_ptr(
                self.as_ptr() as _,
                filter_spec.as_ptr(),
                &mut inputs_new,
                &mut outputs_new,
                ptr::null_mut(),
            )
        }
        .upgrade()?;

        // If no error, inputs and outputs pointer are dangling, manually erase
        // them without dropping. Do this because we need to drop inputs and
        // outputs on the error path.
        let _ = inputs.into_raw();
        let _ = outputs.into_raw();

        // ATTENTION: TODO here we didn't bind the AVFilterInOut to the lifetime of the AVFilterGraph
        let new_inputs = inputs_new
            .upgrade()
            .map(|raw| unsafe { AVFilterInOut::from_raw(raw) });
        let new_outputs = outputs_new
            .upgrade()
            .map(|raw| unsafe { AVFilterInOut::from_raw(raw) });
        Ok((new_inputs, new_outputs))
    }

    /// Check validity and configure all the links and formats in the graph.
    pub fn config(&self) -> Result<()> {
        // ATTENTION: This takes immutable reference since it doesn't delete any filter.
        unsafe { ffi::avfilter_graph_config(self.as_ptr() as *mut _, ptr::null_mut()) }
            .upgrade()?;
        Ok(())
    }
}

impl<'graph> AVFilterGraph {
    pub fn create_filter_context(
        &'graph self,
        filter: &AVFilter,
        name: &CStr,
        args: Option<&CStr>,
    ) -> Result<AVFilterContextMut<'graph>> {
        let args_ptr = args.map(|s| s.as_ptr()).unwrap_or(ptr::null());
        let mut filter_context = ptr::null_mut();
        unsafe {
            ffi::avfilter_graph_create_filter(
                &mut filter_context,
                filter.as_ptr(),
                name.as_ptr(),
                args_ptr,
                ptr::null_mut(),
                // ATTENTION: We restrict the API for not removing filter, then
                // we can legally add filter and take mutable reference to it in
                // a filter graph with immutable reference.
                self.as_ptr() as *mut _,
            )
        }
        .upgrade()
        .map_err(|_| RsmpegError::CreateFilterError)?;

        let filter_context = NonNull::new(filter_context).unwrap();

        Ok(unsafe { AVFilterContextMut::from_raw(filter_context) })
    }
}

impl Default for AVFilterGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AVFilterGraph {
    fn drop(&mut self) {
        let mut filter_graph = self.as_mut_ptr();
        unsafe {
            ffi::avfilter_graph_free(&mut filter_graph);
        }
    }
}