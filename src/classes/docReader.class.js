class DocReader {
    constructor(opts) {
        const modalElementId = "modal_" + opts.modalId;
        const path = opts.path;
        const scale = 1;
        const canvas = document.getElementById(modalElementId).querySelector(".pdf_canvas");
        const context = canvas.getContext('2d');
        let pdfDoc = null,
            pageNum = 1,
            pageRendering = false,
            pageNumPending = null,
            zoom = 100;

        this._getPdfJs = async () => {
            if (!window.__pdfjsLibPromise) {
                window.__pdfjsLibPromise = import("./node_modules/pdfjs-dist/legacy/build/pdf.mjs");
            }
            return window.__pdfjsLibPromise;
        };

        this.renderPage = async (num) => {
            if (!pdfDoc) {
                return;
            }
            pageRendering = true;
            const page = await pdfDoc.getPage(num);
            const viewport = page.getViewport({scale});
            canvas.height = viewport.height;
            canvas.width = viewport.width;

            const renderContext = {
                canvasContext: context,
                viewport
            };
            const renderTask = page.render(renderContext);
            await renderTask.promise;
            pageRendering = false;
            if (pageNumPending !== null) {
                const pending = pageNumPending;
                pageNumPending = null;
                this.renderPage(pending);
            }
            document.getElementById(modalElementId).querySelector(".page_num").textContent = num;
        };

        this.queueRenderPage = (num) => {
            if (pageRendering) {
                pageNumPending = num;
            } else {
                this.renderPage(num);
            }
        };

        this.onPrevPage = () => {
            if (pageNum <= 1) {
                return;
            }
            pageNum--;
            this.queueRenderPage(pageNum);
        };

        this.onNextPage = () => {
            if (pageNum >= pdfDoc.numPages) {
                return;
            }
            pageNum++;
            this.queueRenderPage(pageNum);
        };

        this.zoomIn = () => {
            if (zoom >= 200) {
                return;
            }
            zoom = zoom + 10;
            canvas.style.zoom = zoom + "%";
        };

        this.zoomOut = () => {
            if (zoom <= 50) {
                return;
            }
            zoom = zoom - 10;
            canvas.style.zoom = zoom + "%";
        };

        document.getElementById(modalElementId).querySelector(".previous_page").addEventListener('click', this.onPrevPage);
        document.getElementById(modalElementId).querySelector(".next_page").addEventListener('click', this.onNextPage);
        document.getElementById(modalElementId).querySelector(".zoom_in").addEventListener('click', this.zoomIn);
        document.getElementById(modalElementId).querySelector(".zoom_out").addEventListener('click', this.zoomOut);

        this._getPdfJs().then(pdfjsLib => {
            pdfjsLib.GlobalWorkerOptions.workerSrc = "./node_modules/pdfjs-dist/legacy/build/pdf.worker.mjs";
            return pdfjsLib.getDocument(path).promise;
        }).then(pdfDoc_ => {
            pdfDoc = pdfDoc_;
            document.getElementById(modalElementId).querySelector(".page_count").textContent = pdfDoc.numPages;
            return this.renderPage(pageNum);
        }).catch(error => {
            console.error("Failed to initialize PDF reader", error);
        });
    }
}

if (typeof module !== "undefined") {
    module.exports = {
        DocReader
    };
}
