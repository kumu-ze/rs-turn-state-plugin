// Run through playwright-cli run-code --filename tests/ui-autosave.js.
// Serve this repository on 127.0.0.1:5197 before opening tests/ui-harness.html.
async (page) => {
  const frame = page.frameLocator('iframe');
  await frame.getByRole('heading', {name:'打标管理', exact:true}).waitFor();
  await page.evaluate(() => { window.fixture.delay=1200; });
  await frame.getByRole('switch', {name:'有调用才自动打票',exact:true}).press('Space');
  await page.waitForFunction(() => window.fixture.writes.length===1);
  // Another edit while the first response is outstanding must survive and save next.
  await frame.getByRole('spinbutton', {name:'Fixture account 自定义长度',exact:true}).fill('320');
  await page.waitForFunction(() => window.fixture.panel.settings.accounts.fixture.targetLength===320);
  if(await frame.getByRole('spinbutton', {name:'Fixture account 自定义长度',exact:true}).inputValue()!=='320') throw Error('save response overwrote newer input');
  if(!await page.evaluate(()=>window.fixture.panel.settings.activityOnly)) throw Error('first edit lost');
  await frame.getByRole('status').filter({hasText:'修改自动保存'}).waitFor();
  // Invalid intermediate input remains editable across automatic polling.
  await page.evaluate(() => { window.fixture.delay=0; });
  const input=frame.getByRole('spinbutton',{name:'Fixture account 自定义长度',exact:true});
  await input.fill('3');
  const reads=await page.evaluate(()=>window.fixture.reads);
  await page.waitForFunction(count=>window.fixture.reads>count,reads);
  if(await input.inputValue()!=='3') throw Error('refresh overwrote invalid draft');
  if(!await input.evaluate(element=>element===document.activeElement)) throw Error('refresh stole focus');
  await frame.getByRole('status').filter({hasText:'目标长度应为'}).waitFor();
  // Failed write preserves the edit; manual retry remains available.
  await page.evaluate(() => { window.fixture.fail=true; });
  await input.fill('321');
  await frame.getByRole('status').filter({hasText:'保存失败，草稿已保留'}).waitFor();
  if(await input.inputValue()!=='321') throw Error('failed write lost draft');
  await page.evaluate(() => { window.fixture.fail=false; });
  await frame.getByRole('button',{name:'立即保存',exact:true}).click();
  await page.waitForFunction(()=>window.fixture.panel.settings.accounts.fixture.targetLength===321);
  await frame.getByRole('status').filter({hasText:'修改自动保存'}).waitFor();
  // Another browser changed the revision: retain our draft and do not overwrite it.
  await input.fill('3');
  await page.evaluate(() => { window.fixture.panel.revision++;window.fixture.panel.settings.idleSeconds=180; });
  const reads2=await page.evaluate(()=>window.fixture.reads);
  await page.waitForFunction(count=>window.fixture.reads>count,reads2);
  await input.fill('322');
  await frame.getByRole('status').filter({hasText:'其他页面已更新策略'}).waitFor();
  if(await page.evaluate(()=>window.fixture.panel.settings.accounts.fixture.targetLength)!==321) throw Error('overwrote newer revision');
  await frame.getByRole('button',{name:'重载已保存策略',exact:true}).click();
  await frame.getByRole('button',{name:'放弃草稿并重载',exact:true}).click();
  await frame.getByRole('status').filter({hasText:'修改自动保存'}).waitFor();
  if(await input.inputValue()!=='321') throw Error('explicit reload failed');
  await frame.getByRole('alertdialog').waitFor({state:'hidden'});
  await page.setViewportSize({width:1440,height:1100});
  await page.screenshot({path:'../output/playwright/turn-state-autosave-desktop.png',fullPage:true});
  await page.setViewportSize({width:390,height:844});
  await page.screenshot({path:'../output/playwright/turn-state-autosave-mobile.png',fullPage:true});
  return {passed:true,autosave:true,inFlightEditPreserved:true,pollPreservesDraftAndFocus:true,failureRetry:true,conflictProtected:true};
}
