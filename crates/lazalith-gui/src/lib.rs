//! Step 73: the Lazen GUI library, as Lazen source.
//!
//! # Why this is text in a crate and not a compiled library
//!
//! The same reason the standard library is: a widget set that is *generated* code
//! is a widget set that can disagree with the compiler about what the language
//! means. This goes through the same frontend, lowering and code generation as a
//! user program, so a mistake in `menu_at` is a mistake anyone can point at in a
//! file anyone can read.
//!
//! # What it is built on, and what it refuses to know
//!
//! Every function here is written on `std::graphics` and `std::input`. Nothing
//! here names a window handle, a device, an event queue, or SDL3. That is the
//! whole point of the step: the layer above the SDK is a library, and the layer
//! below it is a host, and neither one leaks into this.
//!
//! # Widgets are geometry, not objects
//!
//! Lazen v1 has no structs, no methods and no enums, so a widget cannot be a
//! value with fields. What it can be is its own geometry, passed to a function
//! that draws it. So there is no `Button` value: there is a rectangle, and
//! `draw_button`.
//!
//! This is a real limitation and it is worth saying what it costs. A widget
//! cannot carry behaviour, so a program that wants a button to do something on
//! release rather than on press writes that itself. In exchange, a widget is
//! three words of arguments rather than an allocation, and there is no lifetime
//! to get wrong.
//!
//! # The argument budget, which shapes every signature here
//!
//! The ABI passes at most **six argument words**, and two of the types a widget
//! needs are two words each: a `&mut [u8]` canvas is an address and a length,
//! and a `&str` is the same. So a drawing function has already spent four of its
//! six words on *what* to draw on and *what* to write, and has two left.
//!
//! That is why every signature below packs its geometry. A canvas's width and
//! height are one word (`pack_surface`), a rectangle is one word
//! (`std::graphics::pack_rect`), a position and a colour are one word
//! (`std::graphics::pack_ink`). The alternative is a widget set where half the
//! calls are refused at the ABI, and a function the compiler accepted but no
//! program can call is worse than one that does not exist.
//!
//! One consequence is stated rather than hidden: **`draw_canvas` blits over the
//! whole destination canvas.** A source view and a destination rectangle are two
//! words between them, and with the canvas and the source there is no seventh
//! word to be had. A program that wants a viewport smaller than its window
//! draws the border with `draw_panel` and blits into the window, which is what
//! v1 can honestly offer.
//!
//! # The two rules everything else follows from
//!
//! - **A widget is drawn clipped, and says whether it drew anything.** Every
//!   `draw_*` returns a `bool` that is false when the widget fell entirely
//!   outside the canvas. A program that lays out more than fits can therefore
//!   *see* that it did rather than having drawn nothing and not known.
//! - **A hit test answers with a number, and the count is the "not found".** This
//!   is the convention `std::input::find` already uses, and repeating it here
//!   means a caller never has to learn a second one: an index below the count is
//!   a hit, and the count is not.

/// The GUI library's module, composed after the standard library.
pub const GUI: &str = r#"
mod gui {
    // ------------------------------------------------------------------
    // A private helper
    //
    // v1's convention for a value that comes back is a `&mut [u8]` the caller
    // owns, and `read_u64` takes a `&[u8]`. Reading one out-parameter back
    // therefore needs a `&[u8]` over the same eight bytes, and doing that inline
    // at each of the places that read a length would put the address arithmetic
    // in every one of them. This is that arithmetic, once.
    // ------------------------------------------------------------------

    /// The `u64` written into `out` by the last call that wrote one there.
    ///
    /// A short `out` reads as zero rather than reading past it, so a caller that
    /// handed over fewer than eight bytes gets "nothing was written" instead of a
    /// fault. A program that declared a `[u8; 4]` where the convention says eight
    /// bytes gets a field that reads as empty rather than a machine that stops.
    fn load_u64(out: &mut [u8]) -> u64 {
        if out.len() as u64 < 8u64 {
            return 0u64;
        }
        return rt::sys::read_u64(
            rt::memory::slice(out.as_mut_slice().as_ptr() as u64, 8u64),
            0u64
        );
    }

    /// A view of a source canvas: the origin to read from, and its pixel width.
    ///
    /// Three 16-bit fields, so it is one word and a `draw_canvas` signature fits
    /// the ABI. The source's *height* is not here because it follows from the
    /// source's length and its width, and a caller states the width only once.
    pub fn pack_view(x: u32, y: u32, width: u32) -> u64 {
        return std::graphics::pack_rect(x, y, width, 0u32);
    }

    /// The `x` a view reads from.
    pub fn view_x(view: u64) -> u32 {
        return std::graphics::rect_x(view);
    }

    /// The `y` a view reads from.
    pub fn view_y(view: u64) -> u32 {
        return std::graphics::rect_y(view);
    }

    /// The source's pixel width.
    pub fn view_width(view: u64) -> u32 {
        return std::graphics::rect_w(view);
    }

    // ------------------------------------------------------------------
    // The window
    //
    // `std::graphics::open` is the one SDK function that takes a record from
    // the caller, because the window's geometry comes back in it. The ABI wants
    // that record word-aligned and an array of bytes has an alignment of one, so
    // a program calling `open` directly can be refused for a reason it cannot
    // see. A GUI program should not have to know that, so `open` here allocates
    // the record itself and the caller never sees one.
    //
    // The geometry the caller already knows is the geometry that comes back, so
    // nothing is lost by not handing the record on. A program that wants to
    // *read* the window's reported geometry is using `std::graphics` directly,
    // which is the right level for that.
    // ------------------------------------------------------------------

    /// Opens a window over `framebuffer`, which the caller owns.
    pub fn open(width: u32, height: u32, framebuffer: &mut [u8]) -> bool {
        let mut words: [u64; 3] = [0u64, 0u64, 0u64];
        let mut record: &mut [u8] = rt::memory::slice_mut(
            words.as_mut_slice().as_ptr() as u64,
            std::graphics::record_bytes()
        );
        return std::graphics::open(width, height, framebuffer, record);
    }

    /// Presents the frame in `framebuffer`, leaving the frame count in `count`.
    pub fn present(framebuffer: &mut [u8], count: &mut [u8]) -> bool {
        return std::graphics::present(framebuffer, count);
    }

    // ------------------------------------------------------------------
    // Geometry
    // ------------------------------------------------------------------

    /// Whether `rect` contains `point`.
    ///
    /// The rectangle is half-open on its far edges — a widget at x = 0 with a
    /// width of 8 covers columns 0 to 7 — so two widgets laid out edge to edge
    /// do not both claim the column between them. A hit test that disagreed with
    /// the drawing about where an edge was would make a button that is drawn
    /// pressable one pixel to its left.
    pub fn contains(rect: u64, point: u64) -> bool {
        let x: u32 = std::graphics::point_x(point);
        let y: u32 = std::graphics::point_y(point);
        if x < std::graphics::rect_x(rect) { return false; }
        if y < std::graphics::rect_y(rect) { return false; }
        if x >= std::graphics::rect_x(rect) + std::graphics::rect_w(rect) { return false; }
        if y >= std::graphics::rect_y(rect) + std::graphics::rect_h(rect) { return false; }
        return true;
    }

    // ------------------------------------------------------------------
    // Layout
    //
    // A layout is a cursor: a position and a direction, packed into one word so
    // a program can carry it as a value. It is not a tree and it is not a
    // manager — a program calls `layout_step` and gets the next position back.
    // Lazen v1 has no structs, so anything richer would be an array the program
    // indexes by hand, which is the same work with more places to get it wrong.
    // ------------------------------------------------------------------

    /// Down: each step moves to the next row.
    pub fn layout_down() -> u32 {
        return 0u32;
    }

    /// Across: each step moves to the next column.
    pub fn layout_across() -> u32 {
        return 1u32;
    }

    /// A layout cursor at (`x`, `y`) moving in `direction`.
    pub fn layout_begin(x: u32, y: u32, direction: u32) -> u64 {
        return std::graphics::pack_point(x, y)
            + (direction as u64) * 281474976710656u64;
    }

    /// The `x` of a layout cursor.
    pub fn layout_x(cursor: u64) -> u32 {
        return std::graphics::point_x(cursor);
    }

    /// The `y` of a layout cursor.
    pub fn layout_y(cursor: u64) -> u32 {
        return std::graphics::point_y(cursor);
    }

    /// The direction a layout cursor moves in.
    pub fn layout_direction(cursor: u64) -> u32 {
        return ((cursor / 281474976710656u64) % 65536u64) as u32;
    }

    /// The cursor after a widget of `extent` pixels, separated by `gap`.
    ///
    /// `extent` is the widget's own size along the direction: its height going
    /// down and its width going across. A layout that never advanced would put
    /// every widget in the same place, and one that advanced by a fixed amount
    /// would have to know every widget's size in advance, which is the thing a
    /// layout exists to avoid.
    pub fn layout_step(cursor: u64, extent: u32, gap: u32) -> u64 {
        if layout_direction(cursor) == layout_down() {
            return std::graphics::pack_point(
                layout_x(cursor),
                layout_y(cursor) + extent + gap
            );
        }
        return std::graphics::pack_point(
            layout_x(cursor) + extent + gap,
            layout_y(cursor)
        );
    }

    // ------------------------------------------------------------------
    // Panel
    // ------------------------------------------------------------------

    /// Fills `rect` with `fill`, clipped to the canvas.
    ///
    /// Returns whether anything was drawn. A panel is the cheapest widget there
    /// is — one `fill_rect` — so it is also the one a program uses for a
    /// background, and a background that fell off the canvas is worth knowing
    /// about.
    pub fn draw_panel(
        canvas: &mut [u8],
        canvas_size: u64,
        rect: u64,
        fill: u32
    ) -> bool {
        if std::graphics::rect_w(rect) == 0u32 { return false; }
        if std::graphics::rect_h(rect) == 0u32 { return false; }
        if std::graphics::rect_x(rect) >= std::graphics::surface_width(canvas_size) {
            return false;
        }
        if std::graphics::rect_y(rect) >= std::graphics::surface_height(canvas_size) {
            return false;
        }
        std::graphics::fill_rect(
            canvas,
            std::graphics::surface_width(canvas_size),
            std::graphics::surface_height(canvas_size),
            rect,
            fill
        );
        return true;
    }

    // ------------------------------------------------------------------
    // Label
    // ------------------------------------------------------------------

    /// Draws `text` with its top-left corner at the position in `ink`.
    ///
    /// `ink` is a packed position and colour — `std::graphics::pack_ink` — and
    /// `canvas_size` is a packed width and height, `std::graphics::pack_surface`.
    /// Packing them is what keeps the call inside the ABI's six argument words;
    /// see the module documentation for why that is the constraint it is.
    ///
    /// `canvas_size` is the *canvas*, not the text. `draw_text` uses the surface
    /// as the bounds every glyph is clipped against, so passing the string's own
    /// extent there would clip the text to a box at the canvas origin rather than
    /// to the canvas — which is a label that vanishes the moment it is placed
    /// away from (0, 0) and looks like a font problem rather than an argument
    /// one.
    ///
    /// The label's own size comes from the text rather than from the caller,
    /// because a label's width is a property of its text and a caller that
    /// supplied it would have to count the characters to get a label the right
    /// size — which is the work this function exists to do.
    ///
    /// The built-in font is eight pixels wide and eight tall, and a code with no
    /// glyph draws nothing rather than a neighbour's, so an unknown character is
    /// a blank cell rather than a wrong picture.
    pub fn draw_label(
        canvas: &mut [u8],
        canvas_size: u64,
        ink: u64,
        text: &str
    ) -> bool {
        if std::text::len(text) == 0u64 { return false; }
        let x: u32 = std::graphics::ink_x(ink);
        let y: u32 = std::graphics::ink_y(ink);
        if x >= std::graphics::surface_width(canvas_size) { return false; }
        if y >= std::graphics::surface_height(canvas_size) { return false; }
        std::graphics::draw_text(canvas, canvas_size, ink, text);
        return true;
    }

    // ------------------------------------------------------------------
    // Button
    //
    // A button's colours are the library's, which is a decision and not an
    // oversight: theming a button needs two more words than the ABI has, and a
    // widget library whose every call is refused is not one. A program that wants
    // its own colours draws the face with `draw_panel` and the text with
    // `draw_label` at `button_label_at`, and the two functions exist for exactly
    // that.
    // ------------------------------------------------------------------

    /// The face a button is drawn with.
    pub fn button_face() -> u32 {
        return std::graphics::rgba(96u8, 104u8, 112u8, 255u8);
    }

    /// The face a *held* button is drawn with.
    ///
    /// A held button is a different colour rather than a shifted label, because
    /// shifting the label needs a second geometry word and this needs none: the
    /// caller passes the same rectangle and a different face.
    pub fn button_face_held() -> u32 {
        return std::graphics::rgba(64u8, 72u8, 80u8, 255u8);
    }

    /// The colour a button's text is drawn in.
    pub fn button_ink() -> u32 {
        return std::graphics::white();
    }

    /// Where a button's label goes, for text `length` bytes long.
    ///
    /// Centred, because a button's text is part of the button's shape and text
    /// that moved when the label changed length would make two buttons of
    /// different labels look like different kinds of thing. A label wider than
    /// the button is pinned to the left edge rather than given a negative
    /// offset, which is what an unsigned subtraction here would produce.
    pub fn button_label_at(rect: u64, length: u64) -> u64 {
        let text_width: u32 = (length * 8u64) as u32;
        let mut left: u32 = std::graphics::rect_x(rect);
        if text_width < std::graphics::rect_w(rect) {
            left = left + (std::graphics::rect_w(rect) - text_width) / 2u32;
        }
        let mut top: u32 = std::graphics::rect_y(rect);
        if std::graphics::rect_h(rect) > 8u32 {
            top = top + (std::graphics::rect_h(rect) - 8u32) / 2u32;
        }
        return std::graphics::pack_point(left, top);
    }

    /// Draws a button: its face, and its label centred inside it.
    pub fn draw_button(
        canvas: &mut [u8],
        canvas_size: u64,
        rect: u64,
        text: &str
    ) -> bool {
        if !draw_panel(canvas, canvas_size, rect, button_face()) { return false; }
        let where: u64 = button_label_at(rect, std::text::len(text));
        draw_label(
            canvas,
            canvas_size,
            std::graphics::pack_ink(
                std::graphics::point_x(where),
                std::graphics::point_y(where),
                button_ink()
            ),
            text
        );
        return true;
    }

    /// Draws a held button: the same, in the held face.
    pub fn draw_button_held(
        canvas: &mut [u8],
        canvas_size: u64,
        rect: u64,
        text: &str
    ) -> bool {
        if !draw_panel(canvas, canvas_size, rect, button_face_held()) { return false; }
        let where: u64 = button_label_at(rect, std::text::len(text));
        draw_label(
            canvas,
            canvas_size,
            std::graphics::pack_ink(
                std::graphics::point_x(where),
                std::graphics::point_y(where),
                button_ink()
            ),
            text
        );
        return true;
    }

    /// Whether a mouse press inside `rect` appears in `events`.
    ///
    /// This is a *click*, not a press-and-hold: the event has to be a mouse-down
    /// and the point has to be inside the rectangle. A program that wants the
    /// held state asks for it itself from the same events, because a widget
    /// library that tracked held state would have to own a per-frame cache, and
    /// the events are already there.
    pub fn button_clicked(events: &[u8], count: u32, rect: u64) -> bool {
        return event_point_in(events, count, rect);
    }

    // ------------------------------------------------------------------
    // Input
    // ------------------------------------------------------------------

    /// The point of the first mouse-down in `events`, packed.
    ///
    /// Returns `(0, 0)` when there is none, which is a real position and
    /// therefore an answer a caller has to be able to *disambiguate*. So this is
    /// not what `button_clicked` is built on: it asks whether a press landed
    /// inside a rectangle, which is false for no press and for a press
    /// elsewhere, and those two do not need telling apart.
    pub fn click_point(events: &[u8], count: u32) -> u64 {
        let mut at: u32 = 0u32;
        while at < count {
            if std::input::is(events, at as u64, std::input::mouse_down()) {
                return std::graphics::pack_point(
                    std::input::x_of(events, at as u64) as u32,
                    std::input::y_of(events, at as u64) as u32
                );
            }
            at = at + 1u32;
        }
        return std::graphics::pack_point(0u32, 0u32);
    }

    /// Whether any mouse-down in `events` landed inside `rect`.
    fn event_point_in(events: &[u8], count: u32, rect: u64) -> bool {
        let mut at: u32 = 0u32;
        while at < count {
            if std::input::is(events, at as u64, std::input::mouse_down()) {
                if contains(rect, click_point(events, 1u32)) { return true; }
            }
            at = at + 1u32;
        }
        return false;
    }

    // ------------------------------------------------------------------
    // Text input
    //
    // A single-line field: a byte buffer, its length, and the two editing
    // actions v1 can express. There is no cursor, so the caret is always at the
    // end and insertion appends. A field with a cursor in the middle needs
    // somewhere to put the cursor between calls, and the only place v1 offers is
    // another out-parameter — which would make every call site declare one and
    // read it back, for a feature a first field does not need.
    // ------------------------------------------------------------------

    /// Appends a character, returning whether it fitted.
    ///
    /// A code outside the font's range is refused rather than appended. The field
    /// is drawn with the built-in font, so a code with no glyph would be a
    /// character the user can type and never see, and a field that silently
    /// swallows a keystroke is worse than one that refuses it.
    pub fn text_input_insert(buffer: &mut [u8], length: &mut [u8], character: u32) -> bool {
        if character < std::graphics::font_first() { return false; }
        if character >= std::graphics::font_last() { return false; }
        let used: u64 = load_u64(length);
        if used + 1u64 > buffer.len() as u64 { return false; }
        buffer[used as usize] = character as u8;
        std::core::write_u64_to(length, used + 1u64);
        return true;
    }

    /// Removes the last character, returning whether there was one.
    pub fn text_input_backspace(buffer: &mut [u8], length: &mut [u8]) -> bool {
        let used: u64 = load_u64(length);
        if used == 0u64 { return false; }
        std::core::write_u64_to(length, used - 1u64);
        return true;
    }

    /// The field's text as a view over its own buffer.
    ///
    /// The view borrows `buffer`, so the two must not outlive each other. An
    /// `ok` of false means the bytes are not valid UTF-8, and the returned view
    /// is empty rather than a string that does not exist.
    pub fn text_input_text(buffer: &[u8], length: &mut [u8], ok: bool) -> &str {
        let used: u64 = load_u64(length);
        if used > buffer.len() as u64 { return ""; }
        return rt::text::from_bytes(
            rt::memory::slice(buffer.as_ptr() as u64, used),
            ok
        );
    }

    /// Draws a text field: its face and its text.
    ///
    /// Whether to draw a caret is the program's decision, and `draw_caret` is
    /// separate for that reason: a widget library that decided focus would have to
    /// own a focus order, and focus order is a program's decision.
    pub fn draw_text_input(
        canvas: &mut [u8],
        canvas_size: u64,
        rect: u64,
        text: &str
    ) -> bool {
        if !draw_panel(canvas, canvas_size, rect, std::graphics::black()) { return false; }
        let where: u64 = button_label_at(rect, std::text::len(text));
        draw_label(
            canvas,
            canvas_size,
            std::graphics::pack_ink(
                std::graphics::point_x(where) + 1u32,
                std::graphics::point_y(where),
                std::graphics::white()
            ),
            text
        );
        return true;
    }

    /// Draws a caret at `at`, six pixels tall.
    pub fn draw_caret(canvas: &mut [u8], canvas_size: u64, at: u64) -> bool {
        return draw_panel(
            canvas,
            canvas_size,
            std::graphics::pack_rect(
                std::graphics::point_x(at),
                std::graphics::point_y(at),
                1u32,
                6u32
            ),
            std::graphics::white()
        );
    }

    // ------------------------------------------------------------------
    // Canvas
    //
    // A window onto another canvas: where to read from, and how wide the source
    // is. That is the whole of a scrolled view, and it is one word because the
    // ABI has room for one.
    // ------------------------------------------------------------------

    /// Copies the part of `source` at `view` over the whole destination canvas.
    ///
    /// The copy is clipped at both ends: a source origin past the source's edge
    /// draws nothing rather than reading memory that is not the source's, and
    /// the destination's own edges are the caller's to have sized. A canvas that
    /// scrolled past its own edge is a normal state, not an error.
    ///
    /// A source of zero width, or one whose width does not divide its length, is
    /// refused rather than guessed at. A blit from a source whose geometry the
    /// caller got wrong would read the wrong bytes and draw them, which is the
    /// one failure here that is silent.
    pub fn draw_canvas(
        canvas: &mut [u8],
        canvas_size: u64,
        source: &[u8],
        view: u64
    ) -> bool {
        let source_width: u32 = view_width(view);
        if source_width == 0u32 { return false; }
        if (source.len() as u64) % (source_width as u64) != 0u64 { return false; }
        let source_height: u32 = ((source.len() as u64) / (source_width as u64)) as u32;
        let width: u32 = std::graphics::surface_width(canvas_size);
        let height: u32 = std::graphics::surface_height(canvas_size);
        if width == 0u32 { return false; }
        if height == 0u32 { return false; }
        let mut row: u32 = 0u32;
        let mut drew: bool = false;
        while row < height {
            let mut column: u32 = 0u32;
            while column < width {
                let source_x: u32 = view_x(view) + column;
                let source_y: u32 = view_y(view) + row;
                if source_x < source_width && source_y < source_height {
                    let colour: u32 = std::graphics::get_pixel(
                        source,
                        source_width,
                        source_height,
                        source_x,
                        source_y
                    );
                    if std::graphics::put_pixel(
                        canvas,
                        width,
                        height,
                        std::graphics::pack_point(column, row),
                        colour
                    ) {
                        drew = true;
                    }
                }
                column = column + 1u32;
            }
            row = row + 1u32;
        }
        return drew;
    }

    // ------------------------------------------------------------------
    // Menu
    //
    // A bar of equal columns. The items' *text* is the program's, not the
    // library's: a menu is a row of rectangles and an index, and a program
    // draws its own labels into the rectangles `menu_item` hands back. The
    // alternative — a menu that takes a list of strings — would need a list of
    // strings, and v1 has no array of `str`.
    // ------------------------------------------------------------------

    /// A menu's state: how many items, and which is selected.
    ///
    /// The bar's colour is *not* here. It is a 32-bit value, and two 16-bit
    /// fields plus 32 bits is 64 — one bit too wide, so packing it would have
    /// meant the colour and the selection overlapping, and a menu with the
    /// second item selected would have drawn as if the colour were a very dark
    /// selection. The colour is a drawing parameter instead, which is also where
    /// it belongs: it is not state, and state is what a caller reads back.
    pub fn pack_menu(items: u32, selected: u32) -> u64 {
        return (items as u64) * 65536u64 + (selected as u64);
    }

    /// How many items a menu has.
    pub fn menu_items(menu: u64) -> u32 {
        return ((menu / 65536u64) % 65536u64) as u32;
    }

    /// Which item of a menu is selected.
    pub fn menu_selected(menu: u64) -> u32 {
        return (menu % 65536u64) as u32;
    }

    /// A menu with `selected` chosen.
    pub fn menu_select(menu: u64, selected: u32) -> u64 {
        return pack_menu(menu_items(menu), selected);
    }

    /// The rectangle of item `index` of a menu filling `rect`.
    ///
    /// An index at or past the item count returns a zero-sized rectangle rather
    /// than refusing, so a caller that walks off the end draws nothing and does
    /// not have to check first. `menu_at` is the one that answers with a number.
    pub fn menu_item(rect: u64, items: u32, index: u32) -> u64 {
        if items == 0u32 { return std::graphics::pack_rect(0u32, 0u32, 0u32, 0u32); }
        if index >= items { return std::graphics::pack_rect(0u32, 0u32, 0u32, 0u32); }
        let each: u32 = std::graphics::rect_w(rect) / items;
        return std::graphics::pack_rect(
            std::graphics::rect_x(rect) + each * index,
            std::graphics::rect_y(rect),
            each,
            std::graphics::rect_h(rect)
        );
    }

    /// The item of a menu under `point`, or the item count if none is.
    ///
    /// The count rather than a sentinel, so a caller can tell "not found" from
    /// "found at the last index" without a second value. This is the convention
    /// `std::input::find` already uses.
    pub fn menu_at(rect: u64, items: u32, point: u64) -> u32 {
        let mut index: u32 = 0u32;
        while index < items {
            if contains(menu_item(rect, items, index), point) { return index; }
            index = index + 1u32;
        }
        return items;
    }

    /// The item a click chose, or the item count if the click missed the bar.
    ///
    /// This is the whole of a menu's input: a menu is a bar and a selection, so
    /// answering "which item was clicked" is the only question a program can ask
    /// without owning an event loop. A press that missed the bar answers with
    /// the count, which is the same answer as no press at all — a menu with
    /// nothing selected is what both of those mean.
    pub fn menu_clicked(events: &[u8], count: u32, rect: u64, items: u32) -> u32 {
        let mut at: u32 = 0u32;
        while at < count {
            if std::input::is(events, at as u64, std::input::mouse_down()) {
                return menu_at(
                    rect,
                    items,
                    std::graphics::pack_point(
                        std::input::x_of(events, at as u64) as u32,
                        std::input::y_of(events, at as u64) as u32
                    )
                );
            }
            at = at + 1u32;
        }
        return items;
    }

    /// Draws a menu bar with its selected item marked, and says whether it drew.
    ///
    /// The bar is the background and the mark is the selected item; the items'
    /// labels are the program's, drawn into the rectangles `menu_item` returns.
    /// A menu that drew its own text would have to own a font, and the font is
    /// an SDK resource that a host does not have — which is the reason it is not
    /// here.
    pub fn draw_menu(
        canvas: &mut [u8],
        canvas_size: u64,
        rect: u64,
        menu: u64,
        bar: u32
    ) -> bool {
        if !draw_panel(canvas, canvas_size, rect, bar) { return false; }
        if menu_selected(menu) < menu_items(menu) {
            draw_panel(
                canvas,
                canvas_size,
                menu_item(rect, menu_items(menu), menu_selected(menu)),
                std::graphics::rgba(64u8, 96u8, 160u8, 255u8)
            );
        }
        return true;
    }
}
"#;
