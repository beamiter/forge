#[path = "../../../src/eva_companion_paint.rs"]
mod paint;
use eva_character_atlas as atlas;
fn main() {
    let mut frames = Vec::new();
    for pose in atlas::Pose::ALL {
        for skin in atlas::Skin::ALL {
            let mut r = atlas::Request::new(skin, pose);
            r.motion = atlas::Motion::Full;
            r.elapsed_ms = 350;
            r.seed = 7;
            let frame = atlas::sample(r).unwrap();
            frames.push(serde_json::json!({"skin":skin.key(),"pose":pose.key(),"seed":r.seed,"elapsed_ms":r.elapsed_ms,"frame":frame.frame_index,"markup":paint::markup(&frame,skin,false),"alt":frame.accessible_label}));
        }
    }
    println!("{}", serde_json::to_string(&frames).unwrap());
}

#[cfg(test)] mod geometry {include!(concat!(env!("OUT_DIR"),"/geometry.rs"));
 #[test] fn measured_native_100_150_200_percent_admissions_use_real_host_selector(){
  let data:serde_json::Value=serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"),"/../../qa/eva-native/measurements.json")).unwrap()).unwrap();
  let frames=data["frames"].as_array().unwrap();assert_eq!(frames.len(),312);
  for f in frames {let dpi=f["dpi"].as_i64().unwrap() as i32;let size=(1000*dpi/96,800*dpi/96);let full=(f["width_px"].as_i64().unwrap() as i32+2,f["height_px"].as_i64().unwrap() as i32+2);let compact=(100*dpi/96,72*dpi/96);let input=Rect{x:0.,y:(size.1-70) as f64,width:size.0 as f64,height:70.};let approval=Rect{x:0.,y:0.,width:(120*dpi/96) as f64,height:(55*dpi/96) as f64};let (r,is_compact)=admit(size,&[input,approval],full,compact).unwrap();assert!(!r.overlaps(input));assert!(!r.overlaps(approval));assert_eq!((r.width as i32,r.height as i32),if is_compact{compact}else{full});assert!(admit(size,&[Rect{x:0.,y:0.,width:size.0 as f64,height:size.1 as f64}],full,compact).is_none());}
 }
}

#[cfg(test)] use companion_protocol as host;
#[cfg(test)] #[path="../../../src/eva_companion_scope.rs"] mod scope_rules;
