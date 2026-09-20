"""Offline regression: post-commit reporting errors do not misreport publication."""
import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('publish_admin_static',Path(__file__).with_name('publish_admin_static.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

class PublishTest(unittest.TestCase):
    def test_report_failure_keeps_explicit_published_result(self):
        for failure in ('local','remote'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as tmp:
                root=Path(tmp);dist=root/'apps/admin-ui/dist';(dist/'assets').mkdir(parents=True)
                index=b'<script src="/admin/assets/app.js"></script>'
                (dist/'index.html').write_bytes(index);(dist/'assets/app.js').write_bytes(b'app')
                dest='/opt/kiro-byok/releases/test/apps/admin-ui/dist';writes={}
                class SSH:
                    def open_sftp(self):return contextlib.nullcontext(self)
                    def open(self,*args):return io.BytesIO(b'old index')
                    def close(self):pass
                class Session:
                    def get(self,url,timeout):
                        class Response:
                            status_code=401 if url.endswith('/cards') else 200
                            content=b'app' if url.endswith('.js') else index
                            def raise_for_status(self):pass
                        return Response()
                def run(ssh,command):
                    if command.startswith('docker inspect'):return json.dumps([{'Source':dest,'Destination':'/app/admin-ui'}])
                    if command.startswith('sha256sum'):return m.hashlib.sha256(b'app').hexdigest()+' file'
                    return ''
                def write(ssh,path,data):
                    if failure=='remote' and path.endswith('/report.json'):raise OSError('fixture report failure')
                    writes[path]=data
                real_write=Path.write_text
                def local_write(path,*args,**kwargs):
                    if failure=='local' and path.name=='admin-bulk-publish.json':raise OSError('fixture report failure')
                    return real_write(path,*args,**kwargs)
                output=io.StringIO()
                with patch.object(m,'ROOT',root),patch.object(m,'pinned_connection',return_value=SSH()),patch.object(m,'deployment_lock',side_effect=lambda _:contextlib.nullcontext()),patch.object(m,'run',side_effect=run),patch.object(m,'write_remote',side_effect=write),patch.object(m.requests,'Session',Session),patch.object(m.sys,'stdin',io.StringIO('{}')),patch.object(Path,'write_text',local_write),contextlib.redirect_stdout(output):
                    m.main()
                result=json.loads(output.getvalue())
                self.assertEqual(result['status'],'published');self.assertEqual(len(result['warnings']),1)
                self.assertEqual(writes[dest+'/index.html'],index)

if __name__=='__main__':unittest.main()
