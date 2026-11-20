"""Rebuild the evaluated vrp-0 classes; fingerprint every external jar, not just its SNAPSHOT label."""
import argparse
import os
from pathlib import Path
import subprocess
from generate import digest, write_json


def build(source, dependencies, output):
    source,dependencies,output=map(lambda p:Path(p).resolve(),(source,dependencies,output))
    output.mkdir(parents=True,exist_ok=True)
    jars=sorted((dependencies/'lib').rglob('*.jar'))
    if not jars: raise ValueError('provide a built quarkus-app containing the pinned dependencies')
    cp=os.pathsep.join(map(str,jars))
    # Rebuild the complete production source tree. The donor application JAR is
    # deliberately excluded: it supplies dependencies, never vrp-0 classes.
    java=sorted((source/'src/main/java').rglob('*.java'))
    adapter=Path(__file__).parent/'java/LegacyAdapter.java'
    args=['-proc:none','-encoding','UTF-8','-cp',cp,'-d',str(output),'-sourcepath',str(output/'empty-sourcepath')]
    args+=list(map(str,java))+[str(adapter.resolve())]
    response=output/'javac.args'
    response.write_text('\n'.join('"'+a.replace('\\','/')+'"' for a in args),encoding='utf-8')
    subprocess.run(['javac','@'+str(response)],check=True)
    runtime=os.pathsep.join([str(output),cp])
    metadata={'source_commit':subprocess.check_output(['git','-C',str(source),'rev-parse','HEAD'],text=True).strip(),
              'source_status':subprocess.check_output(['git','-C',str(source),'status','--porcelain'],text=True).strip(),
              'source_root':str(source), 'adapter_sha256':digest(adapter.read_bytes()),
              'source_files':{str(p.relative_to(source)):digest(p.read_bytes()) for p in java},
              'jars':{p.name:digest(p.read_bytes()) for p in jars},
              'classes':{str(p.relative_to(output)):digest(p.read_bytes()) for p in sorted(output.rglob('*.class'))},
              'classpath':runtime,'build':'javac JDK21, UTF-8, -proc:none; complete src/main/java; pinned dependency jars, no donor application classes'}
    write_json(output/'build.json',metadata)
    return metadata


if __name__=='__main__':
    p=argparse.ArgumentParser(); p.add_argument('--source',required=True); p.add_argument('--dependencies',required=True); p.add_argument('--output',required=True)
    a=p.parse_args(); build(a.source,a.dependencies,a.output)
