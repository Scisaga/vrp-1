// Build-time only. Use the pinned MapLibre font-maker WASM; no runtime Node.
const fs = require('fs'), path = require('path'), vm = require('vm'), zlib = require('zlib');
const [work, output, first='0', last='65536'] = process.argv.slice(2);
const Module = {
  wasmBinary: fs.readFileSync(path.join(work,'sdfglyph.wasm')),
  onRuntimeInitialized() {
    const font=fs.readFileSync(path.join(work,'NotoSansCJKsc-Regular.otf'));
    const stack=Module.ccall('create_fontstack','number',['string'],['VRP Sans']);
    const pointer=Module._malloc(font.length);Module.HEAPU8.set(font,pointer);
    Module.ccall('fontstack_add_face',null,['number','number','number'],[stack,pointer,font.length]);
    fs.mkdirSync(output,{recursive:true});
    for(let start=Number(first);start<Number(last);start+=256) {
      const glyph=Module.ccall('generate_glyph_buffer','number',['number','number'],[stack,start]);
      const data=Module.ccall('glyph_buffer_data','number',['number'],[glyph]);
      const size=Module.ccall('glyph_buffer_size','number',['number'],[glyph]);
      const bytes=Buffer.from(Module.HEAPU8.subarray(data,data+size));
      fs.writeFileSync(path.join(output,`${start}-${start+255}.pbf`),zlib.gzipSync(bytes,{level:9}));
      Module.ccall('free_glyph_buffer',null,['number'],[glyph]);
      if(start%4096===0) process.stdout.write(`Generated glyphs ${start}/65536\n`);
    }
    Module.ccall('free_fontstack',null,['number'],[stack]);
  }
};
vm.runInNewContext(fs.readFileSync(path.join(work,'sdfglyph.js'),'utf8'),{
  Module,require,process,console,Buffer,__dirname:work,TextDecoder,TextEncoder,
  setTimeout,clearTimeout,WebAssembly,
});
