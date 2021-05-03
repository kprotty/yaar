// Copyright (c) 2021 kprotty
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License

#[macro_export]
macro_rules! container_of {
    ($ptr:expr, $container:path, $field:ident) => {{
        let field_ptr = ($ptr as *const _ as *const u8);
        let base_ptr = field_ptr.sub(offset_of!($container, $field));
        base_ptr as *const $container
    }};
}

#[macro_export]
macro_rules! offset_of {
    ($parent:path, $field:tt) => {{
        #[allow(unused_unsafe)]
        unsafe {
            let $parent { $field: _, .. };
            let base_ptr = core::mem::align_of::<$parent>() as *const $parent;
            let field_ptr = core::ptr::addr_of!((*(base_ptr)).$field);
            (field_ptr as usize) - (base_ptr as usize)
        }
    }};
}
