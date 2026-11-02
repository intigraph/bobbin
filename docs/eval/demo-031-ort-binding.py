import argparse, importlib.util, os, pathlib, subprocess, sys, tempfile
parser=argparse.ArgumentParser(description="Synthetic before/after ORT binding; no native execution")
parser.add_argument('--base', required=True, help='Git revision before the ORT-binding change')
args=parser.parse_args()
w=pathlib.Path(__file__).resolve().parents[2];sys.path.insert(0,str(w/'eval'))
from runner import index_reuse as candidate
baseline_dir=tempfile.TemporaryDirectory()
baseline_file=pathlib.Path(baseline_dir.name)/'index_reuse.py'
baseline_file.write_bytes(subprocess.check_output(['git','-C',str(w),'show',args.base+':eval/runner/index_reuse.py']))
spec=importlib.util.spec_from_file_location('baseline',baseline_file);baseline=importlib.util.module_from_spec(spec);spec.loader.exec_module(baseline)
with tempfile.TemporaryDirectory() as temp:
 p=pathlib.Path(temp);ws=p/'source';ws.mkdir();subprocess.run(['git','init','-q',str(ws)],check=True)
 (ws/'app.py').write_text('x = 1\n');subprocess.run(['git','-C',str(ws),'add','app.py'],check=True);subprocess.run(['git','-C',str(ws),'-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture'],check=True)
 os.environ['XDG_CACHE_HOME']=str(p/'cache');model=p/'cache/bobbin/models/fixture';(model/'onnx').mkdir(parents=True);(model/'onnx/model.onnx').write_bytes(b'not executable');(model/'tokenizer.json').write_text('{}')
 (ws/'.bobbin').mkdir();(ws/'.bobbin/config.toml').write_text('quipu_push_chunks=false\nquipu_push_inferred=false\n[index]\nentities=false\ndocuments=false\nmultimodal=false\n[embedding]\nbackend="onnx"\nmodel="fixture"\ngpu=false\n')
 binary=p/'fake-bobbin';binary.write_bytes(b'\x7fELF synthetic identity only');ort=p/'fake-ort';ort.write_bytes(b'qualified synthetic library');os.environ['ORT_DYLIB_PATH']=str(ort)
 record={'source':'synthetic','model':'fixture','ort_sha256':candidate.digest(ort),'assets':{name:candidate.digest(model/name) for name in ['onnx/model.onnx','tokenizer.json']}}
 for module in [baseline,candidate]:module.SUPPORTED_RUNTIMES[candidate.digest(binary)]=record
 print('Target: synthetic files; no executable, model or provider call.')
 for label,module in [('base',baseline),('candidate',candidate)]:
  module.identity(ws,binary,model,None);print(label+': pinned-library positive control ACCEPTED')
 ort.write_bytes(b'changed synthetic library')
 for label,module in [('base',baseline),('candidate',candidate)]:
  try:module.identity(ws,binary,model,None)
  except candidate.ReuseRefused as error:print(label+': changed library REFUSED: '+str(error))
  else:print(label+': changed library ACCEPTED')

baseline_dir.cleanup()
