// Skills stage — inside #zoneBigCenter (the main container,
// container/big_center_container/big_center_container.tsx, which shows one
// thing at a time). skills.js reveals it when the composer's Skills icon is
// clicked. ONE panel serves both tabs: COMPUTER USE lists the host platform's
// AutoCua_data/skills/<mac|windows>/*.md, MOBILE USE lists
// AutoCua_data/skills/ios/*.md (/api/skills?platform=ios). Each row previews
// on click (Edit / Save in the preview), the bin deletes, and + opens the add
// form (Computer asks for a link or application, Mobile for the app's name
// and bundle id) whose Save writes the .md (named after the app / domain) AND
// registers it in skills.json. This was skills_stage.html, injected by
// skills.js; skills.js still wires and drives it.
export function SkillsStage() {
    return (
        <div className="skills-stage" id="skillsStage" aria-label="Skills">

            <div className="skills-stage-inner">

                {/* Tabs row: Computer use / Mobile use (segmented control) with
                    "+ Add skills" just to its right and Close on the far right. */}
                <div className="skills-tabs-row">
                    <div className="skills-tabs" role="tablist" aria-label="Skill target">
                        <button className="skills-tab active" data-tab="computer" role="tab" aria-selected="true" type="button">
                            <svg className="skills-tab-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><rect x="3" y="4" width="18" height="12" rx="1" /><path d="M7 20h10" /><path d="M9 16v4" /><path d="M15 16v4" /></svg>
                            <span>Computer use</span>
                        </button>
                        <button className="skills-tab" data-tab="mobile" role="tab" aria-selected="false" type="button">
                            <svg className="skills-tab-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><rect x="5.5" y="2" width="13" height="20" rx="2.5" /><path d="M10.5 18h3" /></svg>
                            <span>Mobile use</span>
                        </button>
                    </div>
                    <button className="skills-add" type="button" aria-label="Add skills">
                        <span className="skills-add-circle" aria-hidden="true">
                            <svg className="skills-add-plus" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" aria-hidden="true"><path d="M12 5v14" /><path d="M5 12h14" /></svg>
                        </span>
                        <span className="skills-add-label">Add skills</span>
                    </button>
                    {/* Close: puts the screen back exactly the way it was before Skills
                        opened. Same handler as clicking the composer icon again. */}
                    <button className="skills-close" id="skillsCloseBtn" type="button" aria-label="Close skills" title="Close">
                        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true"><path d="M18 6L6 18" /><path d="M6 6l12 12" /></svg>
                    </button>
                </div>

                {/* The panel: three sub-views swapped by skills.js: LIST (default),
                    PREVIEW (one file, read-only until Edit), ADD (via +). */}
                <div className="skills-form active" data-form="skills" role="tabpanel">

                    {/* LIST: one row per file; click the name to preview, bin deletes */}
                    <div className="skills-view active" data-view="list">
                        <div className="skills-list" id="skillsList" aria-label="Saved skills"></div>
                    </div>

                    {/* PREVIEW: header = back button + file name + Edit / Save. */}
                    <div className="skills-view" data-view="preview">
                        <div className="skills-preview-head">
                            <button className="skills-back" type="button" aria-label="Back to skills list" title="Back">
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M15 18l-6-6 6-6" /></svg>
                            </button>
                            <span className="skills-preview-name" id="skillsPreviewName"></span>
                            <div className="skills-preview-actions">
                                <button className="skills-edit-btn" id="skillsEditBtn" type="button">Edit</button>
                                <button className="skills-save-btn" id="skillsSaveBtn" type="button" hidden>Save</button>
                            </div>
                        </div>
                        <pre className="skills-preview-body" id="skillsPreviewBody"></pre>
                        <textarea className="skills-preview-body skills-preview-editbox" id="skillsPreviewEdit" hidden spellCheck={false}></textarea>
                    </div>

                    {/* ADD: the field row differs per tab; the skill text and the
                        buttons are shared. Save = POST /api/skills[?platform=ios]. */}
                    <div className="skills-view" data-view="add">
                        <div className="skills-row skills-add-fields" data-for="computer">
                            <div className="skills-field">
                                <label className="skills-field-label" htmlFor="skillsAddTarget">Link / Application</label>
                                <input type="text" className="skills-input" id="skillsAddTarget" placeholder="https://…  or  application name (the file is named after it)" autoComplete="off" />
                            </div>
                        </div>
                        <div className="skills-row skills-add-fields" data-for="mobile" hidden>
                            <div className="skills-field">
                                <label className="skills-field-label" htmlFor="skillsAddApp">Application</label>
                                <input type="text" className="skills-input" id="skillsAddApp" placeholder="name on the home screen, e.g. Instagram" autoComplete="off" />
                            </div>
                            <div className="skills-field">
                                <label className="skills-field-label" htmlFor="skillsAddBundle">Bundle id</label>
                                <input type="text" className="skills-input" id="skillsAddBundle" placeholder="optional, e.g. com.burbn.instagram" autoComplete="off" />
                            </div>
                        </div>
                        <div className="skills-field skills-field-grow">
                            <label className="skills-field-label" htmlFor="skillsAddText">Skill</label>
                            <textarea className="skills-textarea" id="skillsAddText" placeholder="Write the skill here…"></textarea>
                        </div>
                        <div className="skills-actions">
                            <span className="skills-add-error" id="skillsAddError" role="alert"></span>
                            <button className="skills-cancel" type="button">Cancel</button>
                            <button className="skills-save" id="skillsAddSave" type="button">Save</button>
                        </div>
                    </div>

                </div>

            </div>
        </div>
    );
}
